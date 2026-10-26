#!/usr/bin/env python3
"""Actual X11 pixels/input, authority and virtual audio; no product fixture hooks."""
import ctypes as c
import json
import os
from pathlib import Path
import re
import resource
import shutil
import subprocess as sp
import tempfile
import time
from PIL import Image
from native_graphics_controls import aim_pitch,component_profile,run_graphics

ROOT = Path(__file__).resolve().parents[1]
ARTIFACTS = ROOT / ".local/artifacts"
ARTIFACTS.mkdir(exist_ok=True)
X = c.CDLL("libX11.so.6")
T = c.CDLL("libXtst.so.6")
X.XOpenDisplay.argtypes = [c.c_char_p]
X.XOpenDisplay.restype = c.c_void_p
X.XDefaultRootWindow.argtypes = [c.c_void_p]
X.XDefaultRootWindow.restype = c.c_ulong
X.XQueryTree.argtypes = [c.c_void_p, c.c_ulong, c.POINTER(c.c_ulong), c.POINTER(c.c_ulong), c.POINTER(c.POINTER(c.c_ulong)), c.POINTER(c.c_uint)]
X.XFetchName.argtypes = [c.c_void_p, c.c_ulong, c.POINTER(c.c_char_p)]
X.XFree.argtypes = [c.c_void_p]
X.XFlush.argtypes = [c.c_void_p]
X.XSetInputFocus.argtypes = [c.c_void_p, c.c_ulong, c.c_int, c.c_ulong]
X.XStringToKeysym.argtypes = [c.c_char_p]
X.XStringToKeysym.restype = c.c_ulong
X.XKeysymToKeycode.argtypes = [c.c_void_p, c.c_ulong]
X.XKeysymToKeycode.restype = c.c_ubyte
X.XCloseDisplay.argtypes = [c.c_void_p]
T.XTestFakeRelativeMotionEvent.argtypes = [c.c_void_p,c.c_int,c.c_int,c.c_ulong]
T.XTestFakeMotionEvent.argtypes = [c.c_void_p, c.c_int, c.c_int, c.c_int, c.c_ulong]
T.XTestFakeButtonEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]
T.XTestFakeKeyEvent.argtypes = [c.c_void_p, c.c_uint, c.c_int, c.c_ulong]

def wait(predicate, label, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(label)

def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except sp.TimeoutExpired:
            process.kill()
            process.wait()

with tempfile.TemporaryDirectory(prefix="native-product-", dir=ROOT / ".local/tmp") as folder:
    folder = Path(folder)
    display = next(f":{number}" for number in range(121, 151) if not Path(f"/tmp/.X11-unix/X{number}").exists() and not Path(f"/tmp/.X{number}-lock").exists())
    config = folder / "dummy.conf"
    config.write_text('''Section "ServerFlags"
 Option "AutoAddDevices" "false"
EndSection
Section "Device"
 Identifier "dummy"
 Driver "dummy"
 VideoRam 256000
EndSection
Section "Monitor"
 Identifier "monitor"
 HorizSync 30.0-80.0
 VertRefresh 60.0-75.0
 Modeline "1280x720" 74.50 1280 1344 1472 1664 720 723 728 748
EndSection
Section "Screen"
 Identifier "screen"
 Device "dummy"
 Monitor "monitor"
 DefaultDepth 24
 SubSection "Display"
  Depth 24
  Modes "1280x720"
 EndSubSection
EndSection
''')
    alsa = folder / "alsa.conf"
    alsa.write_text("pcm.!default { type null }\n")
    environment = {**os.environ, "DISPLAY": display, "VK_ICD_FILENAMES": str(ROOT / ".local/mesa/lvp-local.json"), "ALSA_CONFIG_PATH": str(alsa), "CUBI_AUDIO_VIRTUAL": "1", "CUBI_UI_DIAGNOSTICS": "1", "XDG_CONFIG_HOME": str(folder / "config")}
    processes = []
    dpy = None
    try:
        with (folder / "xorg.stderr").open("w") as output:
            processes.append(sp.Popen(["Xorg", display, "-noreset", "-nolisten", "tcp", "-ac", "-config", str(config), "-logfile", str(folder / "Xorg.log")], stdout=output, stderr=sp.STDOUT, cwd=ROOT))
        dpy = wait(lambda: X.XOpenDisplay(display.encode()), "owned Xorg unavailable", 10)
        ready = folder / "ready"
        with (folder / "server.log").open("w") as output:
            processes.append(sp.Popen([os.environ.get("CUBI_PRODUCT_SERVER",str(ROOT / "target/debug/server")), "--data", str(folder / "world"), "--ws", "127.0.0.1:0", "--udp", "127.0.0.1:0", "--ready-file", str(ready), "--spawn", "8.5,90,8.5"], stdout=output, stderr=sp.STDOUT, cwd=ROOT, env=environment))
        wait(ready.exists, "authority unavailable")
        address = re.search(r"ws=(\S+)", ready.read_text())[1]
        owner = os.environ.get("CUBI_UI_OWNER") == "1"
        if owner:
            identity_hash=0xcbf29ce484222325
            canonical_address=address if "://" in address else "ws://"+address
            for byte in canonical_address.encode():identity_hash=((identity_hash^byte)*0x100000001b3)&0xffffffffffffffff
            identity_path=folder/f"config/CubiCyber/identity-{identity_hash:016x}.cubi"
            identity_path.parent.mkdir(parents=True,exist_ok=True)
            shutil.copyfile(folder/"world/owner.identity",identity_path)
            identity_path.chmod(0o600)
        log = folder / "native.log"
        stack_kib = int(os.environ.get("CUBI_NATIVE_STACK_KIB", "0"))
        def native_limits():
            if stack_kib:
                resource.setrlimit(resource.RLIMIT_STACK, (stack_kib*1024, resource.getrlimit(resource.RLIMIT_STACK)[1]))
        with log.open("w") as output:
            processes.append(sp.Popen([os.environ.get("CUBI_PRODUCT_NATIVE",str(ROOT / "target/debug/client-native")), "--software"], stdout=output, stderr=sp.STDOUT, cwd=ROOT, env=environment, preexec_fn=native_limits))
        native = processes[-1]
        def text():
            if native.poll() is not None:
                raise AssertionError(log.read_text())
            return log.read_text()
        wait(lambda: "NATIVE_UI_READY" in text(), "native Welcome unavailable")
        def find_window(window):
            name = c.c_char_p()
            if X.XFetchName(dpy, window, c.byref(name)) and name.value:
                value = name.value.decode(errors="replace")
                X.XFree(name)
                if value == "Cubi Cyber":
                    return window
            root, parent, children, count = c.c_ulong(), c.c_ulong(), c.POINTER(c.c_ulong)(), c.c_uint()
            if X.XQueryTree(dpy, window, c.byref(root), c.byref(parent), c.byref(children), c.byref(count)):
                found = next((result for child in children[:count.value] if (result := find_window(child))), None)
                if children:
                    X.XFree(children)
                return found
        window = wait(lambda: find_window(X.XDefaultRootWindow(dpy)), "actual native window unavailable")
        X.XSetInputFocus(dpy, window, 1, 0)
        def key(name, down=None):
            code = X.XKeysymToKeycode(dpy, X.XStringToKeysym(name.encode()))
            assert code, name
            for pressed in ([True, False] if down is None else [down]):
                T.XTestFakeKeyEvent(dpy, code, pressed, 0)
            X.XFlush(dpy)
            time.sleep(0.012)
        def type_text(value):
            names = {".": "period", "/": "slash", ";": "semicolon", " ": "space", "-": "minus"}
            for char in value:
                shift = char == ":" or char.isupper()
                if shift:
                    key("Shift_L", True)
                key("semicolon" if char == ":" else names.get(char, char.lower()))
                if shift:
                    key("Shift_L", False)
        def click(x, y,button=1):
            T.XTestFakeMotionEvent(dpy, -1, x, y, 0)
            T.XTestFakeButtonEvent(dpy, button, True, 0)
            T.XTestFakeButtonEvent(dpy, button, False, 0)
            X.XFlush(dpy)
            time.sleep(0.25)
        def painted(screen, after=0, extra=""):
            return wait(lambda: re.search(r"NATIVE_UI_PAINTED screen="+screen+r"[^\n]*"+extra, text()[after:]), "native UI paint missing: "+screen)
        def capture(name, require_ui=True):
            states=list(re.finditer(r"NATIVE_UI_STATE [^\n]* window=([0-9a-f]+) [^\n]*window_revision=(\d+)",text()))
            if states:
                stamp=states[-1];wanted_window,wanted=stamp.groups();after_state=stamp.start()
                wait(lambda:re.search(r"NATIVE_UI_PAINTED [^\n]* window="+wanted_window+r" [^\n]*window_revision="+wanted+r"(?:\n|$)",text()[after_state:]),"confirmed window state has not reached UI upload",30)
            revisions=re.findall(r"NATIVE_UI_PAINTED [^\n]* revision=(\d+)",text())
            assert revisions,"no uploaded UI revision"
            expected=int(revisions[-1])
            wait(lambda:any(int(value)>=expected for value in re.findall(r"NATIVE_UI_PRESENTED revision=(\d+)",text())),"actual UI GPU completion missing",30)
            time.sleep(0.1)
            path = ARTIFACTS / f"native-product-{name}.png"
            sp.run(["import", "-window", str(window), str(path)], env=environment, check=True, stdout=sp.DEVNULL, stderr=sp.PIPE)
            image = Image.open(path).convert("RGB")
            bright = sum(r > 200 and g > 190 and b > 170 for r, g, b in image.get_flattened_data())
            if require_ui:assert bright > 1000, (name, bright)
            return bright
        welcome = capture("welcome")
        click(385, 315)
        painted(r"Settings\(Graphics\)")
        settings = capture("settings")
        before=len(text())
        key("Escape")
        painted("Welcome",before)
        component_settings=None
        if os.environ.get("CUBI_UI_LOW_EFFECTS")=="1":
            assert os.environ.get("CUBI_UI_COMPONENT_ONLY")=="1","low effects requires explicit component-only scope"
            def setup_wheel():
                T.XTestFakeMotionEvent(dpy,-1,450,260,0)
                T.XTestFakeButtonEvent(dpy,5,True,0);T.XTestFakeButtonEvent(dpy,5,False,0);X.XFlush(dpy)
            component_settings=component_profile(text=text,click=click,key=key,painted=painted,wheel=setup_wheel,wait=wait)
        click(450, 229)
        for _ in range(50):
            key("BackSpace")
        type_text(address)
        click(480, 275)
        if os.environ.get("CUBI_UI_DIAGNOSTIC_ONLY") == "1":
            deadline=time.monotonic()+30
            while time.monotonic()<deadline:
                if "NATIVE_UI_STATE screen=Game" in text():break
                time.sleep(.5)
            started=time.time_ns();key("F8")
            diagnostic=wait(lambda:next((path for path in (ARTIFACTS/"flight").glob("native-*.json") if path.stat().st_mtime_ns>=started),None),"loading F8 report missing",15)
            shutil.copyfile(diagnostic,ARTIFACTS/"native-loading-flight.json")
            print("NATIVE_LOADING_DIAGNOSTICS "+json.dumps({"acceptance":False,"lastStates":[line for line in text().splitlines() if line.startswith(("NATIVE_UI_STATE","NATIVE_UI_DERIVED","NATIVE_UI_WORLD","NATIVE_UI_LIGHT","NATIVE_UI_CPU_CONTINUATION"))][-15:]}))
            raise SystemExit(0)
        wait(lambda: "screen=Game" in text(), "real native server scene unavailable", 120)
        component_only=os.environ.get("CUBI_UI_COMPONENT_ONLY")=="1"
        far_pattern="[0-9]+" if component_only else "[1-9][0-9]*"
        profile = wait(lambda: re.search(r"NATIVE_UI_WORLD light_pages=([1-9][0-9]*) far_tiles=("+far_pattern+r") environment=true authoritative=true eye_sky=Some\(([0-9]+)\)", text()), "confirmed authoritative LIGHT/far cover unavailable", 120)
        if component_settings:
            def effective_receipt():
                rows=re.findall(r"NATIVE_UI_SETTINGS applied=(\d+) pending=(\d+) reload=(\d+)",text())
                return int(rows[-1][0]) if rows and int(rows[-1][0])&0x1e00==0x1e00 else None
            applied_mask=wait(effective_receipt,"actual low-effects backend receipts missing",30)
            component_settings["controlReceipts"]={"ssao":True,"reflections":True,"clouds":True,"gi":True,"horizonCommitted":bool(applied_mask&1),"appliedMask":applied_mask}
        wait(lambda: "ping_measured=true" in text(), "native measured ping unavailable")
        key("e")
        wait(lambda: re.search(r"screen=Inventory window=0*[1-9a-f][0-9a-f]*", text()), "guarded native inventory unavailable")
        painted("Inventory",extra=r"window=0*[1-9a-f][0-9a-f]*")
        inventory = capture("inventory")
        key("e")
        key("t")
        type_text("native authoritative chat")
        key("Return")
        wait(lambda: "chat_messages=1" in text(), "native chat not acknowledged")
        before=len(text())
        crafted=False
        chest_transfer=False
        configurator_cycle=False
        creative_grant=False
        if owner:
            assert "role=Owner" in text()
            def window_state(after=0,**expected):
                rows=[dict((name,int(value)) for name,value in re.findall(r"(\w+)=(\d+)",line)) for line in text()[after:].splitlines() if line.startswith("NATIVE_UI_WINDOW ")]
                return rows[-1] if rows and all(rows[-1].get(name)==value for name,value in expected.items()) else None
            def command(value):
                before=len(text());key("t");painted("Game",before,extra="focus=Chat")
                type_text(value);key("Return")
                wait(lambda:"NATIVE_UI_INTENT class=command" in text()[before:],"physical command did not reach the typed intent",30)
            command("/give 9 1")
            wait(lambda:window_state(inventory_items=1),"owner give never reached inventory")
            before=len(text());key("e");painted("Inventory",before,extra=r"window=0*[1-9a-f][0-9a-f]*")
            before=len(text());click(191,411)
            wait(lambda:window_state(before,cursor_item=9,cursor_count=1,inventory_items=0),"log was not picked up authoritatively")
            before=len(text());click(286,157)
            wait(lambda:window_state(before,craft0_item=9,craft0_count=1,cursor_count=0,result_item=40,result_count=4),"real log recipe result missing")
            painted("Inventory",before)
            capture("crafting")
            image=Image.open(ARTIFACTS/"native-product-crafting.png").convert("RGB")
            assert sum(r>200 and g>190 and b>170 for r,g,b in image.crop((434,195,472,211)).get_flattened_data())>3,"craft result quantity has no visible glyph"
            before=len(text());click(453,192)
            wait(lambda:window_state(before,cursor_item=40,cursor_count=4,craft0_count=0,result_count=0),"craft output was not exactly four planks")
            before=len(text());click(191,411)
            wait(lambda:window_state(before,inventory_items=4,cursor_count=0),"crafted planks not returned to owned inventory")
            key("e");painted("Game")
            crafted=True
            before=len(text());command("/give 52 1")
            wait(lambda:window_state(before,inventory_items=5),"owner chest missing")
            before=len(text());key("2")
            wait(lambda:window_state(before,selected=1),"chest slot selection not authoritative")
            click(480,270)
            def aim_motion(dx,dy):
                T.XTestFakeRelativeMotionEvent(dpy,dx,dy,0);X.XFlush(dpy);time.sleep(.15)
            before=len(text());aim_pitch(target=60,motion=aim_motion,text=text,wait=wait)
            wait(lambda:re.search(r"NATIVE_UI_AIM target=Some.*cell=[1-9]",text()[before:]),"real downward aim missing")
            time.sleep(0.3)
            def right_click():
                T.XTestFakeButtonEvent(dpy,3,True,0);X.XFlush(dpy);time.sleep(0.08);T.XTestFakeButtonEvent(dpy,3,False,0);X.XFlush(dpy);time.sleep(0.25)
            before=len(text());right_click()
            wait(lambda:window_state(before,inventory_items=4),"owned chest was not placed/consumed")
            wait(lambda:"cell=52" in text()[before:],"placed chest not under actual ray")
            wait(lambda:"NATIVE_UI_BLOCK_AUDIO accepted_cues=" in text()[before:],"accepted placement did not reach actual audio endpoint")
            before=len(text());right_click();painted("Container",before,extra=r"window=0*[1-9a-f][0-9a-f]*")
            capture("chest")
            before=len(text());click(191,411)
            wait(lambda:window_state(before,cursor_item=40,cursor_count=4,inventory_items=0),"chest transfer did not take own planks")
            before=len(text());click(191,155)
            wait(lambda:window_state(before,cursor_count=0,container_items=4),"chest deposit not durable/authoritative")
            before=len(text());key("e");painted("Game",before)
            T.XTestFakeButtonEvent(dpy,1,True,0);T.XTestFakeButtonEvent(dpy,1,False,0);X.XFlush(dpy);time.sleep(0.15)
            before=len(text());right_click();painted("Container",before,extra=r"window=0*[1-9a-f][0-9a-f]*")
            wait(lambda:window_state(before,container_items=4,cursor_count=0),"reopened chest lost/duplicated planks")
            before=len(text());click(191,155)
            wait(lambda:window_state(before,cursor_item=40,cursor_count=4,container_items=0),"chest withdrawal missing")
            before=len(text());click(191,411)
            wait(lambda:window_state(before,inventory_items=4,cursor_count=0,container_items=0),"chest withdrawal not conserved")
            before=len(text());key("e");painted("Game",before)
            chest_transfer=True
            before=len(text());command("/gamemode creative")
            wait(lambda:window_state(before,creative=1),"owner creative state missing")
            before=len(text());key("e");painted("Inventory",before,extra=r"window=0*[1-9a-f][0-9a-f]*")
            click(600,146);before=len(text());type_text("stone");painted("Inventory",before)
            before=len(text());click(581,185)
            wait(lambda:window_state(before,inventory_items=68),"authorized creative NEI grant missing")
            capture("creative")
            before=len(text());key("Escape");painted("Game",before)
            creative_grant=True
            before=len(text());command("/give 327 1")
            wait(lambda:window_state(before,inventory_items=69),"owned configurator missing")
            before=len(text());key("3")
            wait(lambda:"NATIVE_UI_ITEM selected=2 item=327 fact=Some(Configurator(0))" in text()[before:],"owned configurator fact missing")
            def mode_press(mode,back=False):
                before=len(text())
                if back:key("Shift_L",True)
                key("n")
                if back:key("Shift_L",False)
                wait(lambda:f"NATIVE_UI_ITEM selected=2 item=327 fact=Some(Configurator({mode}))" in text()[before:],"authoritative configurator mode did not reach UI")
            mode_press(1)
            mode_press(0,True)
            for mode in [1,2,3,4,0]:mode_press(mode)
            before=len(text());key("e");painted("Inventory",before,extra=r"window=0*[1-9a-f][0-9a-f]*")
            T.XTestFakeMotionEvent(dpy,-1,287,411,0);X.XFlush(dpy);time.sleep(0.2)
            capture("configurator")
            before=len(text());key("Escape");painted("Game",before)
            configurator_cycle=True
        graphics=None
        if os.environ.get("CUBI_UI_GRAPHICS")=="1":
            def motion(dx,dy):
                T.XTestFakeRelativeMotionEvent(dpy,dx,dy,0);X.XFlush(dpy);time.sleep(.15)
            def wheel():
                T.XTestFakeButtonEvent(dpy,5,True,0);T.XTestFakeButtonEvent(dpy,5,False,0);X.XFlush(dpy);time.sleep(.1)
            graphics=run_graphics(text=text,click=click,key=key,painted=painted,capture=capture,motion=motion,wheel=wheel,wait=wait,identity=lambda:next((folder/"config/CubiCyber").glob("identity-*.cubi")).read_bytes(),artifacts=ARTIFACTS)
        before=len(text())
        key("Tab", True)
        painted("Game",before, "tab=true")
        roster = capture("roster")
        key("Tab", False)
        before=len(text())
        key("F3")
        painted("Game",before,"debug=true")
        profiler = capture("profiler")
        key("F3")
        started = time.time_ns()
        key("F8")
        report_path = wait(lambda: next((path for path in (ARTIFACTS / "flight").glob("native-*.json") if path.stat().st_mtime_ns >= started), None), "native F8 missing")
        report = json.loads(report_path.read_text())
        assert report["movementReplayAvailable"] and report["movementReplay"]["steps"] > 0
        assert any(frame.get("gpuTotalComplete") and (frame.get("gpuNs") or 0)>0 for frame in report["frames"]),"no actual complete GPU interval"
        assert report["movementReplay"]["rendererReplay"] is False
        replay = sp.run([os.environ.get("CUBI_PRODUCT_REPLAY",str(ROOT / "target/debug/examples/replay")), str(report_path)], cwd=ROOT, env=environment, capture_output=True, text=True)
        assert replay.returncode == 0, replay.stderr
        assert "MOVEMENT_REPLAY_OK" in replay.stdout
        (ARTIFACTS / "native-product-movement-replay.txt").write_text(replay.stdout)
        voices = [int(value) for value in re.findall(r"voice_samples=(\d+)", text())]
        assert max(voices, default=0) > 0, "real virtual endpoint never mixed a UI sound"
        assert "callback_errors=0" in text()
        before=len(text())
        key("Escape")
        painted("Pause",before)
        click(480, 315)
        wait(lambda: text().count("NATIVE_UI_STATE screen=Welcome") >= 2, "native disconnect unavailable")
        identity = next((folder / "config/CubiCyber").glob("identity-*.cubi")).read_bytes()
        prior_games = text().count("NATIVE_UI_STATE screen=Game")
        painted("Welcome",before)
        click(480, 275)
        wait(lambda: text().count("NATIVE_UI_STATE screen=Game") > prior_games, "native fresh View reconnect unavailable", 120)
        assert next((folder / "config/CubiCyber").glob("identity-*.cubi")).read_bytes() == identity
        before=len(text())
        key("Escape")
        painted("Pause",before)
        click(480, 356)
        wait(lambda: native.poll() is not None, "native UI quit unavailable")
        assert native.returncode == 0, log.read_text()
        result = {"graphics":graphics,"componentSettings":component_settings,"fullProfileAccepted":not component_only,"scope":"UI/audio/replay components; M5 excluded" if component_only else "full product profile","mainStackLimitKiB": stack_kib or None, "productProfile":{"authoritativeLightPages":int(profile[1]),"farHeightTiles":int(profile[2]),"knownEyeSky":int(profile[3]),"atmospherePost":True,"shadowResolution":2048}, "ownerAuthenticated":owner,"authoritativeCraft":crafted,"authoritativeChestTransfer":chest_transfer,"authorizedCreativeNeiGrant":creative_grant, "welcomePixels": welcome, "settingsPixels": settings, "inventoryPixels": inventory, "rosterPixels": roster, "profilerPixels": profiler, "authority": True, "guardedInventory": True, "chat": True, "serverMeasuredPing": True, "virtualAudioVoiceSamples": max(voices), "hardwareAudibilityProven": False, "movementReplaySteps": report["movementReplay"]["steps"], "offlineReplay": True, "persistedIdentityResumed": True, "freshViewReconnect": True}
        result["authoritativeConfiguratorCycle"]=configurator_cycle
        (ARTIFACTS / (("native-owner-ui-components.json" if owner else "native-product-ui-components.json") if component_only else ("native-owner-ui-smoke.json" if owner else "native-product-ui-smoke.json"))).write_text(json.dumps(result, indent=2) + "\n")
        (ARTIFACTS / ("native-owner-ui-smoke.log" if owner else "native-product-ui-smoke.log")).write_text(log.read_text())
        print(("NATIVE_PRODUCT_UI_COMPONENTS_OK " if component_only else "NATIVE_PRODUCT_UI_OK ") + json.dumps(result))
    finally:
        if "log" in locals() and log.exists():
            (ARTIFACTS/("native-owner-ui-last.log" if locals().get("owner",False) else "native-product-ui-last.log")).write_text(log.read_text())
        server_log=folder/"server.log"
        if server_log.exists():(ARTIFACTS/("native-owner-server-last.log" if locals().get("owner",False) else "native-product-server-last.log")).write_text(server_log.read_text())
        if dpy:
            X.XCloseDisplay(dpy)
        for process in reversed(processes):
            stop(process)
