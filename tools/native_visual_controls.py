"""Real XTest settings clicks; receipts come from the active prepared GPU owners."""
import re
from native_graphics_controls import widget_controls

def run_visual_choices(*,text,click,key,painted,wheel,wait,identity):
    widget=widget_controls(text=text,click=click,wheel=wheel,wait=wait)
    saved_identity=identity()
    counts=re.findall(r"NATIVE_UI_WINDOW [^\n]*inventory_items=(\d+)",text())
    owned=int(counts[-1]) if counts else 0
    observed=[]
    def choose(name,automatic,styles):
        before=len(text());key("Escape");painted("Pause",before)
        before=len(text());widget("Settings");painted(r"Settings\(Graphics\)",before)
        before=len(text());widget(f"Setting({name})",True)
        pattern="NATIVE_UI_VISUAL automatic=Some("+str(automatic).lower()+") styles=Some(["+", ".join(map(str,styles))+"])"
        wait(lambda:"NATIVE_UI_STATE screen=Game" in text()[before:] and pattern in text()[before:],"prepared visual choice missing: "+name,120)
        assert identity()==saved_identity,"visual reprepare changed identity/credential"
        wait(lambda:re.search(r"NATIVE_UI_WINDOW [^\n]*inventory_items="+str(owned)+r"(?: |\n|$)",text()[before:]),"visual reprepare lost owned items")
        painted("Game",before)
        observed.append({"setting":name,"automatic":automatic,"styles":styles,"sameIdentity":True,"ownedItems":owned})
    choose("ExposureMode",False,[0,0,0])
    choose("SkyStyle",False,[1,0,1])
    choose("SkyStyle",False,[2,1,2])
    choose("ExposureMode",True,[2,1,2])
    choose("SkyStyle",True,[0,0,0])
    return {"actualButtons":True,"asynchronousLoading":True,"receipts":observed,"fullProfileAccepted":False}
