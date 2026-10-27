"""Actual XTest controls; observes public diagnostic bounds and completed pixels."""
import re
from pathlib import Path
from PIL import Image

def widget_controls(*,text,click,wheel,wait):
    def bounds(name):
        log=text(); batches=list(re.finditer(r"NATIVE_UI_PAINTED [^\n]*",log))
        if not batches:return None
        start=batches[-2].end() if len(batches)>1 else 0
        # Older diagnostic whitelists omit these two real controls. Locate
        # their adjacent rows from measured controls; only XTest clicks act.
        candidates=[(name,0)]
        if name=="Setting(Hold60)":candidates.extend([("Setting(Exposure)",-72),("Setting(Gi)",36)])
        if name=="Setting(Near)":candidates.append(("Setting(Horizon)",36))
        for candidate,dy in candidates:
            match=re.search(r"NATIVE_UI_WIDGET name="+re.escape(candidate)+r" x=([0-9.e+-]+) y=([0-9.e+-]+) width=([0-9.e+-]+) height=([0-9.e+-]+)",log[start:batches[-1].start()])
            if match:
                x,y,w,h=map(float,match.groups());return x,y+dy,w,h
        return None
    def widget(name,scroll=False,button=1):
        for _ in range(32):
            box=bounds(name)
            if box and (not scroll or 110<box[1]+box[3]/2<460):
                click(round(box[0]+box[2]/2),round(box[1]+box[3]/2),button);return
            if not scroll:
                wait(lambda:bounds(name),"visible control missing: "+name,30)
                continue
            before=len(text());wheel();wait(lambda: "NATIVE_UI_PAINTED" in text()[before:],"scroll paint missing",30)
        raise AssertionError("scrolled control missing: "+name)
    return widget

def component_profile(*,text,click,key,painted,wheel,wait):
    widget=widget_controls(text=text,click=click,wheel=wheel,wait=wait)
    before=len(text());click(385,315);painted(r"Settings\(Graphics\)",before)
    for _ in range(16):
        value=int(re.findall(r"NATIVE_UI_SAVED_EFFECTS [^\n]*horizon=(\d+)",text())[-1])
        if value==16:break
        before=len(text());widget("Setting(Horizon)",False,3)
        wait(lambda:"NATIVE_UI_PAINTED" in text()[before:],"horizon preference paint missing",30)
    assert int(re.findall(r"NATIVE_UI_SAVED_EFFECTS [^\n]*horizon=(\d+)",text())[-1])==16
    for name in ["Ssao","Reflections","Clouds","Gi","Hold60"]:
        before=len(text());widget(f"Setting({name})",True,3)
        wait(lambda:re.search(r"NATIVE_UI_SAVED_EFFECTS [^\n]*"+name.lower()+r"=0(?: |\n)",text()[before:]),"manual component saved choice missing: "+name,30)
    latest=re.findall(r"NATIVE_UI_SAVED_EFFECTS clouds=(\d+) gi=(\d+) reflections=(\d+)",text())[-1]
    assert latest==("0","0","0"),("actual manual low-effects choices missing",latest)
    assert re.findall(r"NATIVE_UI_SAVED_EFFECTS [^\n]*ssao=(\d+)",text())[-1]=="0"
    assert re.findall(r"NATIVE_UI_SAVED_EFFECTS [^\n]*hold60=(\d+)",text())[-1]=="0"
    widget("Back");painted("Welcome",before)
    return {"actualWelcomeButtons":True,"saved":{"horizon":16,"ssao":0,"clouds":0,"gi":0,"reflections":0,"hold60":0}}

def horizon_profile(*,text,click,painted,wheel,wait):
    """Literal1024 route: change only horizon, keep defaultNear32/LodError32."""
    widget=widget_controls(text=text,click=click,wheel=wheel,wait=wait)
    before=len(text());click(385,315);painted(r"Settings\(Graphics\)",before)
    for _ in range(16):
        value=int(re.findall(r"NATIVE_UI_SAVED_EFFECTS [^\n]*horizon=(\d+)",text())[-1])
        if value==1024:break
        before=len(text());widget("Setting(Horizon)")
        wait(lambda:"NATIVE_UI_PAINTED" in text()[before:],"manual1024 horizon paint missing",30)
    assert int(re.findall(r"NATIVE_UI_SAVED_EFFECTS [^\n]*horizon=(\d+)",text())[-1])==1024
    widget("Back");painted("Welcome",before)
    return {"actualWelcomeButtons":True,"horizon":1024,"near":32,"lodError":32,"otherDefaultChoicesRetained":True}

def aim_pitch(*,target,motion,text,wait):
    """Closed-loop real XTest motion; X11 may report a synthetic delta once or twice."""
    before=len(text());motion(0,-2000)
    wait(lambda:"NATIVE_UI_MOTION" in text()[before:],"real mouse motion missing",30)
    for _ in range(16):
        values=re.findall(r"NATIVE_UI_MOTION [^\n]*pitch=([-0-9.e+]+)",text());assert values
        pitch=float(values[-1])
        if abs(pitch-target)<2:return
        # Half correction converges for either one or two raw reports.
        delta=max(-360,min(360,round((target-pitch)/.3)))
        before=len(text());motion(0,delta)
        wait(lambda:"NATIVE_UI_MOTION" in text()[before:],"physical aim correction missing",30)
    raise AssertionError(("actual camera pitch did not settle",pitch,target))

def run_graphics(*, text, click, key, painted, capture, motion, wheel, wait, identity, artifacts):
    widget=widget_controls(text=text,click=click,wheel=wheel,wait=wait)
    def menu(name):
        before=len(text());key("Escape");painted("Pause",before)
        before=len(text());widget(name)
        painted(r"Settings\(Graphics\)" if name=="Settings" else name,before)
    def leave():
        before=len(text());widget("Back");painted("Pause",before)
        before=len(text());key("Escape");painted("Game",before)
    def shot(name):
        capture(name,require_ui=False)
        return Image.open(Path(artifacts)/f"native-product-{name}.png").convert("RGB")
    def difference(a,b):
        a=a.crop((10,10,950,240));b=b.crop((10,10,950,240))
        pixels=list(zip(a.get_flattened_data(),b.get_flattened_data()))
        return sum(max(abs(x-y) for x,y in zip(p,q))>8 for p,q in pixels)
    saved_identity=identity()
    owned_counts=re.findall(r"NATIVE_UI_WINDOW [^\n]*inventory_items=(\d+)",text())
    owned_before=int(owned_counts[-1]) if owned_counts else 0
    aim_pitch(target=35,motion=motion,text=text,wait=wait)
    menu("Crossroads");before=len(text());widget("Day")
    wait(lambda:re.search(r"NATIVE_UI_TIME choice=1 known=true[^\n]*time_of_day=Some\(6000\.0\)",text()[before:]),"day clock missing")
    leave();day=shot("personal-day")
    menu("Crossroads");before=len(text());widget("Night")
    wait(lambda:re.search(r"NATIVE_UI_TIME choice=2 known=true[^\n]*time_of_day=Some\(18000\.0\)",text()[before:]),"night clock missing")
    leave();night=shot("personal-night");changed=difference(day,night)
    assert changed>5000,("actual day/night pixels unchanged",changed)
    menu("Crossroads");widget("Day");leave()
    # Use an unsaturated display-compensation interval. At the default daylight
    # exposure a clipped sky can stay white even after a correct uniform change.
    menu("Settings")
    def exposure():
        stamps=re.findall(r"NATIVE_UI_TIME [^\n]*exposure_percent=(\d+)",text());assert stamps
        return int(stamps[-1])
    for _ in range(35):
        value=exposure()
        if value==25:break
        before=len(text());widget("Setting(Exposure)",True,3)
        wait(lambda:f"exposure_percent={value-5}" in text()[before:],"low exposure fixture upload missing")
    assert exposure()==25,"could not select actual low exposure fixture"
    leave();baseline=shot("exposure-before")
    menu("Settings");initial=exposure();before=len(text())
    for _ in range(5):widget("Setting(Exposure)",True)
    wait(lambda:f"exposure_percent={initial+25}" in text()[before:],"actual exposure upload missing")
    leave();exposed=shot("exposure-after");exposure_changed=difference(baseline,exposed)
    assert exposure_changed>500,("actual exposure pixels unchanged",exposure_changed)
    menu("Settings");before=len(text());widget("Setting(Msaa)",True,3)
    wait(lambda:"NATIVE_UI_STATE screen=Game" in text()[before:] and re.search(r"NATIVE_UI_TIME [^\n]* samples=1 ",text()[before:]) and re.search(r"NATIVE_UI_RENDER [^\n]*smaa=true smaa_bytes=[1-9][0-9]*",text()[before:]),"selected SMAA1x View unavailable",120)
    assert identity()==saved_identity,"graphics reprepare replaced owned identity/credential"
    wait(lambda:re.search(r"NATIVE_UI_WINDOW [^\n]*inventory_items="+str(owned_before)+r"(?: |\n|$)",text()[before:]),"graphics reprepare lost owned inventory")
    shot("msaa-one")
    menu("Settings");before=len(text());widget("Setting(Scale)",True)
    wait(lambda:"NATIVE_UI_STATE screen=Game" in text()[before:] and re.search(r"NATIVE_UI_RENDER scale=85 width=816 height=459",text()[before:]),"actual85 percent HDR extent unavailable",120)
    assert identity()==saved_identity,"scale reprepare replaced owned identity/credential"
    wait(lambda:re.search(r"NATIVE_UI_WINDOW [^\n]*inventory_items="+str(owned_before)+r"(?: |\n|$)",text()[before:]),"scale reprepare lost owned inventory")
    scaled=shot("scale-85");assert scaled.size==(960,540),"UI surface must retain physical extent"
    wait(lambda:re.search(r"NATIVE_UI_NEAR requested=32 effective=32 unsupported=false",text()[before:]),"actual minimum32 near profile unavailable",30)
    menu("Settings");before=len(text());widget("Setting(Near)")
    wait(lambda:re.search(r"NATIVE_UI_NEAR requested=48 effective=32 unsupported=true",text()[before:]),"unsupported Near48 did not retain actual32 profile",30)
    assert "NATIVE_UI_STATE screen=Loading" not in text()[before:],"unsupported near request dropped the active View"
    assert identity()==saved_identity,"unsupported near request replaced owned identity"
    before=len(text());widget("Setting(Near)",False,3)
    wait(lambda:re.search(r"NATIVE_UI_NEAR requested=32 effective=32 unsupported=false",text()[before:]),"supported Near32 did not clear explicit error",30)
    leave()
    menu("Crossroads");widget("ServerTime");leave()
    return {"scope":"graphics controls","fullProfileAccepted":False,"actualDayNightButtons":True,"dayNightChangedPixels":changed,"exposureBefore":initial,"exposureAfter":initial+25,"exposureChangedPixels":exposure_changed,"selectedSamples":1,"smaaPrepared":True,"renderScale":85,"renderExtent":[816,459],"physicalUiExtent":[960,540],"effectiveNearBlocks":32,"unsupportedNear48Retains32":True,"asynchronousLoading":True,"sameIdentity":True,"ownedItemsPreserved":owned_before}
