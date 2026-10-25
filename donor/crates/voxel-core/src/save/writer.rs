//! Поток записи (design.md §2.4–2.5): поток симуляции собирает пачку (Saver::collect_*) за миллисекунды и отдаёт её сюда,
//! а запись на диск (файл на ключ, fsync, rename) идёт здесь — тик её не ждёт. Пачки пишутся по порядку; ошибка записи
//! возвращается текстом (take_errors) — симуляция скажет Saver::failed() и следующая запись будет полной.
//! Только натив: в браузере сервера нет.

use std::sync::mpsc;
use std::thread;

use super::storage::FileStorage;
use super::Batch;

enum Job {
    Batch(Batch),
    /// Все пачки до этой записаны — ответить.
    Sync(mpsc::Sender<()>),
}

pub(crate) struct Writer {
    tx: Option<mpsc::Sender<Job>>,
    errs: mpsc::Receiver<String>,
    th: Option<thread::JoinHandle<()>>,
}

impl Writer {
    /// Поток записи над хранилищем st (его владелец — поток).
    pub(crate) fn spawn(mut st: FileStorage) -> std::io::Result<Writer> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (etx, errs) = mpsc::channel::<String>();
        let th = thread::Builder::new().name("save".into()).spawn(move || {
            for job in rx {
                match job {
                    Job::Batch(b) => {
                        if let Err(e) = b.write(&mut st) {
                            let _ = etx.send(format!("запись мира: {e}"));
                        }
                    }
                    Job::Sync(ack) => {
                        let _ = ack.send(());
                    }
                }
            }
        })?;
        Ok(Writer { tx: Some(tx), errs, th: Some(th) })
    }

    /// Отдать пачку потоку записи (не ждёт). false — поток записи умер: пачка потеряна, симуляции — failed().
    pub(crate) fn submit(&self, b: Batch) -> bool {
        if b.ops.is_empty() {
            return true;
        }
        self.tx.as_ref().is_some_and(|tx| tx.send(Job::Batch(b)).is_ok())
    }

    /// Дождаться записи всего отданного.
    pub(crate) fn sync(&self) -> bool {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx.as_ref().is_some_and(|tx| tx.send(Job::Sync(ack_tx)).is_ok()) && ack_rx.recv().is_ok()
    }

    /// Ошибки записи с прошлого раза.
    pub(crate) fn take_errors(&self) -> Vec<String> {
        self.errs.try_iter().collect()
    }

    /// Дописать очередь и остановить поток. Ответ — ошибки записи, не разобранные раньше.
    pub(crate) fn finish(mut self) -> Vec<String> {
        self.stop();
        self.take_errors()
    }

    fn stop(&mut self) {
        self.tx.take();
        if let Some(th) = self.th.take() {
            let _ = th.join();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop(); // очередь дописывается до конца: сервер упал в Rust-панике — то, что отдано, всё равно ляжет
    }
}
