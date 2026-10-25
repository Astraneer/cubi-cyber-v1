//! Project-local loader compatibility API; no upstream libloading code is used.
//!
//! The supported subset is the one used by the locked ash/wgpu/winit graph:
//! loading a library, borrowing typed symbols, Unix explicit loading flags, and
//! opening an already-loaded Windows module. Loading/symbol ABI safety remains
//! the caller's obligation. Loading work happens at initialization, not per frame.

use std::{
    borrow::Cow,
    ffi::{CStr, CString, FromBytesWithNulError, NulError, OsStr},
    fmt,
    marker::PhantomData,
    mem,
    ops::Deref,
};

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    IncompatibleSize,
    CreateCString {
        source: NulError,
    },
    CreateCStringWithTrailing {
        source: FromBytesWithNulError,
    },
    OpenLibrary {
        source: dlopen2::Error,
    },
    SymbolLookup {
        source: dlopen2::Error,
    },
    GetProcAddress {
        source: std::io::Error,
    },
    GetProcAddressUnknown,
    GetModuleHandleExW {
        source: std::io::Error,
    },
    UnixLoader {
        operation: &'static str,
        message: String,
    },
    InvalidPath,
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompatibleSize => f.write_str("symbol type must have pointer size"),
            Self::CreateCString { source } => write!(f, "invalid symbol name: {source}"),
            Self::CreateCStringWithTrailing { source } => {
                write!(f, "invalid terminated symbol name: {source}")
            }
            Self::OpenLibrary { source } => write!(f, "cannot open library: {source}"),
            Self::SymbolLookup { source } => write!(f, "cannot find symbol: {source}"),
            Self::GetProcAddress { source } => write!(f, "cannot find Windows symbol: {source}"),
            Self::GetProcAddressUnknown => f.write_str("cannot find Windows symbol"),
            Self::GetModuleHandleExW { source } => {
                write!(f, "module is not already loaded: {source}")
            }
            Self::InvalidPath => f.write_str("library path contains a NUL character"),
            Self::UnixLoader { operation, message } => write!(f, "{operation}: {message}"),
            Self::Unsupported(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateCString { source } => Some(source),
            Self::CreateCStringWithTrailing { source } => Some(source),
            Self::OpenLibrary { source } | Self::SymbolLookup { source } => Some(source),
            Self::GetProcAddress { source } | Self::GetModuleHandleExW { source } => Some(source),
            _ => None,
        }
    }
}

fn symbol_name(bytes: &[u8]) -> Result<Cow<'_, CStr>, Error> {
    if bytes.last() == Some(&0) {
        CStr::from_bytes_with_nul(bytes)
            .map(Cow::Borrowed)
            .map_err(|source| Error::CreateCStringWithTrailing { source })
    } else {
        CString::new(bytes)
            .map(Cow::Owned)
            .map_err(|source| Error::CreateCString { source })
    }
}

fn validate_path(path: &OsStr) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if path.as_bytes().contains(&0) {
            return Err(Error::InvalidPath);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        if path.encode_wide().any(|unit| unit == 0) {
            return Err(Error::InvalidPath);
        }
    }
    Ok(())
}

#[derive(Debug)]
enum Backend {
    Loaded(dlopen2::raw::Library),
    #[cfg(unix)]
    ExplicitUnix(unix_module::Module),
    #[cfg(windows)]
    Existing(windows_module::Module),
}

#[derive(Debug)]
pub struct Library {
    backend: Backend,
}

impl Library {
    /// Open a module through the platform loader.
    ///
    /// # Safety
    /// Module initializers and finalizers execute native code. The caller must
    /// trust that code and ensure its initialization/finalization requirements.
    pub unsafe fn new<P: AsRef<OsStr>>(filename: P) -> Result<Self, Error> {
        validate_path(filename.as_ref())?;
        let library = dlopen2::raw::Library::open(filename)
            .map_err(|source| Error::OpenLibrary { source })?;
        Ok(Self {
            backend: Backend::Loaded(library),
        })
    }

    /// Borrow a symbol with its exact ABI/type and keep the module borrowed.
    ///
    /// # Safety
    /// `T` must be a valid pointer-like representation of this symbol with its
    /// exact signature. Calls must obey the foreign function's safety contract.
    /// A copied pointer must never be used after this library has been unloaded.
    ///
    /// ```compile_fail
    /// use libloading::{Library, Symbol};
    /// fn escape(lib: &Library) -> Symbol<'static, unsafe extern "C" fn()> {
    ///     unsafe { lib.get(b"example").unwrap() }
    /// }
    /// ```
    pub unsafe fn get<T>(&self, symbol: &[u8]) -> Result<Symbol<'_, T>, Error> {
        if mem::size_of::<T>() != mem::size_of::<*mut std::ffi::c_void>() {
            return Err(Error::IncompatibleSize);
        }
        let name = symbol_name(symbol)?;
        let pointer = match &self.backend {
            Backend::Loaded(library) => {
                // SAFETY: requesting a raw pointer has exactly pointer size;
                // validation of the eventual typed symbol is the caller's duty.
                unsafe { library.symbol_cstr::<*mut std::ffi::c_void>(&name) }
                    .map_err(|source| Error::SymbolLookup { source })?
            }
            #[cfg(unix)]
            Backend::ExplicitUnix(module) => module.get(&name)?,
            #[cfg(windows)]
            Backend::Existing(module) => module.get(&name)?,
        };
        Ok(Symbol {
            pointer,
            borrow: PhantomData,
        })
    }
}

/// Typed symbol borrowing its module. The pointer slot is stored, rather than
/// constructing/dropping an arbitrary `T`, so this type never owns foreign data.
pub struct Symbol<'library, T> {
    pointer: *mut std::ffi::c_void,
    borrow: PhantomData<&'library T>,
}

impl<T> fmt::Debug for Symbol<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Symbol").field(&self.pointer).finish()
    }
}

impl<T> Clone for Symbol<'_, T> {
    fn clone(&self) -> Self {
        Self {
            pointer: self.pointer,
            borrow: PhantomData,
        }
    }
}

impl<T> Deref for Symbol<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `get` verifies pointer size; its unsafe contract requires the
        // exact valid pointer-like T. The slot is aligned for any pointer-sized
        // T and remains live for the returned reference's borrow of self.
        unsafe { &*((&self.pointer as *const *mut std::ffi::c_void).cast::<T>()) }
    }
}

// SAFETY: a symbol is only an immutable address; its library borrow remains
// alive. Passing its represented value between threads requires T: Send.
unsafe impl<T: Send> Send for Symbol<'_, T> {}
// SAFETY: sharing its represented value between threads requires T: Sync.
unsafe impl<T: Sync> Sync for Symbol<'_, T> {}

pub mod os {
    #[cfg(unix)]
    pub mod unix {
        use super::super::{Backend, Error, Symbol, unix_module::Module};
        use std::ffi::OsStr;

        pub const RTLD_NOW: i32 = libc::RTLD_NOW;
        pub const RTLD_LAZY: i32 = libc::RTLD_LAZY;
        pub const RTLD_LOCAL: i32 = libc::RTLD_LOCAL;
        pub const RTLD_GLOBAL: i32 = libc::RTLD_GLOBAL;

        #[derive(Debug)]
        pub struct Library(pub(super) super::super::Library);

        impl Library {
            /// Open a named module with the exact Unix loader flags.
            ///
            /// # Safety
            /// Loading/finalizing native code must be safe for the caller;
            /// flags must be valid for the platform. A missing filename refers
            /// to the current program, exactly as POSIX dlopen specifies.
            pub unsafe fn open<P: AsRef<OsStr>>(
                filename: Option<P>,
                flags: i32,
            ) -> Result<Self, Error> {
                let filename = filename.as_ref().map(AsRef::as_ref);
                let module = Module::open(filename, flags)?;
                Ok(Self(super::super::Library {
                    backend: Backend::ExplicitUnix(module),
                }))
            }

            pub fn this() -> Result<Self, Error> {
                let library = dlopen2::raw::Library::open_self()
                    .map_err(|source| Error::OpenLibrary { source })?;
                Ok(Self(super::super::Library {
                    backend: Backend::Loaded(library),
                }))
            }

            /// # Safety
            /// The symbol type and all uses must satisfy `Library::get`.
            pub unsafe fn get<T>(&self, symbol: &[u8]) -> Result<Symbol<'_, T>, Error> {
                // SAFETY: exactly the caller's typed-symbol obligations.
                unsafe { self.0.get(symbol) }
            }
        }

        impl From<Library> for super::super::Library {
            fn from(value: Library) -> Self {
                value.0
            }
        }
    }

    #[cfg(windows)]
    pub mod windows {
        use super::super::{Backend, Error, Symbol, windows_module::Module};
        use std::ffi::OsStr;

        #[derive(Debug)]
        pub struct Library(pub(super) super::super::Library);

        impl Library {
            /// Acquire an owning reference to an existing module without
            /// loading any new library or executing new initializers.
            pub fn open_already_loaded<P: AsRef<OsStr>>(filename: P) -> Result<Self, Error> {
                let module = Module::existing(filename.as_ref())?;
                Ok(Self(super::super::Library {
                    backend: Backend::Existing(module),
                }))
            }

            /// # Safety
            /// The symbol type and all uses must satisfy `Library::get`.
            pub unsafe fn get<T>(&self, symbol: &[u8]) -> Result<Symbol<'_, T>, Error> {
                // SAFETY: exactly the caller's typed-symbol obligations.
                unsafe { self.0.get(symbol) }
            }
        }

        impl From<Library> for super::super::Library {
            fn from(value: Library) -> Self {
                value.0
            }
        }
    }
}

#[cfg(unix)]
mod unix_module {
    use super::Error;
    use std::{
        ffi::{CStr, CString, OsStr, c_void},
        os::unix::ffi::OsStrExt,
    };

    #[derive(Debug)]
    pub struct Module {
        handle: *mut c_void,
    }

    fn loader_error(operation: &'static str) -> Error {
        // SAFETY: dlerror returns either NULL or a NUL-terminated loader-owned
        // string. Copy immediately while its thread-local message is valid.
        let raw = unsafe { libc::dlerror() };
        let message = if raw.is_null() {
            "loader returned NULL without an error message".to_owned()
        } else {
            // SAFETY: checked non-null loader string with the documented C ABI.
            unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned()
        };
        Error::UnixLoader { operation, message }
    }

    impl Module {
        pub fn open(filename: Option<&OsStr>, flags: i32) -> Result<Self, Error> {
            let filename = filename
                .map(|name| CString::new(name.as_bytes()).map_err(|_| Error::InvalidPath))
                .transpose()?;
            let pointer = filename
                .as_ref()
                .map_or(std::ptr::null(), |name| name.as_ptr());
            // SAFETY: clear this thread's previous loader error before dlopen.
            unsafe {
                libc::dlerror();
            }
            // SAFETY: filename is NULL or a live NUL-terminated string. The
            // outer unsafe public API requires valid flags and trusted code.
            let handle = unsafe { libc::dlopen(pointer, flags) };
            if handle.is_null() {
                return Err(loader_error("dlopen"));
            }
            Ok(Self { handle })
        }

        pub fn get(&self, name: &CStr) -> Result<*mut c_void, Error> {
            // SAFETY: clear this thread's previous error before symbol lookup.
            unsafe {
                libc::dlerror();
            }
            // SAFETY: this object owns a live loader reference and name remains
            // NUL-terminated for the duration of the native call.
            let pointer = unsafe { libc::dlsym(self.handle, name.as_ptr()) };
            if pointer.is_null() {
                return Err(loader_error("dlsym"));
            }
            Ok(pointer)
        }
    }

    impl Drop for Module {
        fn drop(&mut self) {
            // SAFETY: own exactly one successful dlopen reference; typed
            // symbols borrow this object and cannot outlive it.
            unsafe {
                libc::dlclose(self.handle);
            }
        }
    }

    // SAFETY: Unix loader references support cross-thread use; no mutable
    // library data is exposed without the caller's unsafe typed-symbol check.
    unsafe impl Send for Module {}
    // SAFETY: same loader reference guarantees; symbol types add their bounds.
    unsafe impl Sync for Module {}
}

#[cfg(windows)]
mod windows_module {
    use super::Error;
    use std::{
        ffi::{CStr, OsStr, c_void},
        os::windows::ffi::OsStrExt,
    };
    use windows_sys::Win32::{
        Foundation::{FreeLibrary, HMODULE},
        System::LibraryLoader::{GetModuleHandleExW, GetProcAddress},
    };

    #[derive(Debug)]
    pub struct Module {
        handle: HMODULE,
    }

    impl Module {
        pub fn existing(filename: &OsStr) -> Result<Self, Error> {
            super::validate_path(filename)?;
            let filename: Vec<u16> = filename.encode_wide().chain(Some(0)).collect();
            let mut handle = std::ptr::null_mut();
            // SAFETY: NUL-terminated immutable filename and a valid output
            // pointer. Zero flags increments the loader's reference count.
            let found = unsafe { GetModuleHandleExW(0, filename.as_ptr(), &mut handle) };
            if found == 0 {
                return Err(Error::GetModuleHandleExW {
                    source: std::io::Error::last_os_error(),
                });
            }
            Ok(Self { handle })
        }

        pub fn get(&self, name: &CStr) -> Result<*mut c_void, Error> {
            // SAFETY: handle owns a live loader reference and name is a valid
            // NUL-terminated byte string for the entire native call.
            let pointer = unsafe { GetProcAddress(self.handle, name.as_ptr().cast()) };
            pointer
                .map(|function| function as *mut c_void)
                .ok_or_else(|| Error::GetProcAddress {
                    source: std::io::Error::last_os_error(),
                })
        }
    }

    impl Drop for Module {
        fn drop(&mut self) {
            // SAFETY: this object owns exactly the reference acquired by
            // GetModuleHandleExW; symbols borrow the object and cannot outlive it.
            unsafe {
                FreeLibrary(self.handle);
            }
        }
    }

    // SAFETY: Windows loader handles and reference counting support concurrent
    // lookups. This object does not expose mutable library-owned memory.
    unsafe impl Send for Module {}
    // SAFETY: same platform guarantees as Send; symbol types add their own bounds.
    unsafe impl Sync for Module {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn system_library() -> Library {
        // SAFETY: the operating system's C runtime is trusted, and this test
        // only uses documented stateless standard-C functions.
        unsafe { Library::new("libc.so.6") }.expect("system libc")
    }

    #[cfg(windows)]
    fn system_library() -> Library {
        os::windows::Library::open_already_loaded("kernel32.dll")
            .expect("kernel32 already loaded")
            .into()
    }

    #[test]
    fn bad_sizes_and_names_fail_before_ffi() {
        let lib = system_library();
        // SAFETY: these intentionally invalid requests must fail before a typed
        // symbol is created, never dereference or call an invalid type.
        let bad_size = unsafe { lib.get::<u8>(b"irrelevant") };
        assert!(matches!(bad_size, Err(Error::IncompatibleSize)));
        // SAFETY: same validation-only request; pointer-sized but invalid name.
        let bad_name = unsafe { lib.get::<usize>(b"bad\0name") };
        assert!(matches!(bad_name, Err(Error::CreateCString { .. })));
        // SAFETY: same validation-only request with invalid terminated name.
        let bad_terminated = unsafe { lib.get::<usize>(b"bad\0name\0") };
        assert!(matches!(
            bad_terminated,
            Err(Error::CreateCStringWithTrailing { .. })
        ));
        // SAFETY: missing symbol is never called or dereferenced.
        let missing = unsafe { lib.get::<usize>(b"cubi_missing_symbol_9df01") };
        assert!(missing.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn loads_and_calls_borrowed_c_function_with_both_name_forms() {
        let lib = system_library();
        for name in [b"strlen".as_slice(), b"strlen\0".as_slice()] {
            // SAFETY: strlen's exact C ABI and documented argument below.
            let strlen =
                unsafe { lib.get::<unsafe extern "C" fn(*const std::ffi::c_char) -> usize>(name) }
                    .unwrap();
            let cloned = strlen.clone();
            // SAFETY: static NUL-terminated input valid for the duration of call.
            assert_eq!(unsafe { cloned(c"Cubi".as_ptr()) }, 4);
        }
    }

    #[cfg(all(unix, target_os = "linux"))]
    #[test]
    fn no_load_flags_do_not_load_an_absent_module() {
        // SAFETY: documented Unix flags; intentionally absent module is never loaded.
        let missing = unsafe {
            os::unix::Library::open(
                Some("/cubi/no/such/module.so"),
                libc::RTLD_NOW | libc::RTLD_NOLOAD,
            )
        };
        assert!(missing.is_err());
        let _held = system_library();
        // SAFETY: same flags reference the already loaded system runtime.
        let loaded = unsafe {
            os::unix::Library::open(Some("libc.so.6"), libc::RTLD_NOW | libc::RTLD_NOLOAD)
        }
        .unwrap();
        let _: Library = loaded.into();
    }

    #[cfg(all(unix, target_os = "linux"))]
    #[test]
    fn no_load_preserves_an_existing_module_without_loading_a_new_one() {
        use std::{
            path::PathBuf,
            sync::atomic::{AtomicUsize, Ordering},
        };

        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/loader-fixtures");
        std::fs::create_dir_all(&parent).unwrap();
        let fixture = loop {
            let path = parent.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break Fixture(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create private loader fixture: {error}"),
            }
        };
        let source = fixture.0.join("fixture.c");
        let path = fixture.0.join("fixture.so");
        std::fs::write(&source, "int cubi_fixture_value(void) { return 42; }\n").unwrap();
        let output = std::process::Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(&source)
            .arg("-o")
            .arg(&path)
            .output()
            .expect("C toolchain");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        // SAFETY: valid flags and a real, trusted fixture that must not be loaded.
        let unloaded =
            unsafe { os::unix::Library::open(Some(&path), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        assert!(
            unloaded.is_err(),
            "NOLOAD must reject an existing but unloaded file"
        );
        // SAFETY: this fixture contains only our simple function and no initializers.
        let loaded = unsafe { Library::new(&path) }.unwrap();
        // SAFETY: exact C ABI of the generated fixture function.
        let value =
            unsafe { loaded.get::<unsafe extern "C" fn() -> i32>(b"cubi_fixture_value") }.unwrap();
        // SAFETY: fixture function has no arguments or caller-specific invariants.
        assert_eq!(unsafe { value() }, 42);
        // SAFETY: the module is live; this only increments its loader reference.
        let retained =
            unsafe { os::unix::Library::open(Some(&path), libc::RTLD_NOW | libc::RTLD_NOLOAD) }
                .unwrap();
        drop(loaded);
        // SAFETY: exact fixture ABI; retained module owns the remaining reference.
        let value = unsafe { retained.get::<unsafe extern "C" fn() -> i32>(b"cubi_fixture_value") }
            .unwrap();
        // SAFETY: no arguments or caller-specific invariants.
        assert_eq!(unsafe { value() }, 42);
        drop(retained);
        // SAFETY: after both references closed, NOLOAD must not reload the file.
        let closed =
            unsafe { os::unix::Library::open(Some(&path), libc::RTLD_NOW | libc::RTLD_NOLOAD) };
        assert!(
            closed.is_err(),
            "all owned loader references must be released"
        );
    }

    #[cfg(windows)]
    #[test]
    fn existing_windows_module_and_missing_module_follow_reference_semantics() {
        let module = os::windows::Library::open_already_loaded("kernel32.dll").unwrap();
        // SAFETY: exact Windows ABI; GetCurrentProcessId takes no arguments.
        let get_pid =
            unsafe { module.get::<unsafe extern "system" fn() -> u32>(b"GetCurrentProcessId") }
                .unwrap();
        // SAFETY: no arguments or caller-specific invariants.
        assert_eq!(unsafe { get_pid() }, std::process::id());
        assert!(os::windows::Library::open_already_loaded("cubi-not-loaded-791e.dll").is_err());
    }
}
