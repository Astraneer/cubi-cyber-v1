// Observe one atomic UI-stamp/completion snapshot. Never fence the latest
// unrelated Scene submission or install an additional GPU callback.
export const UI_PRESENTATION_SNAPSHOT = `(()=>{const s=window.__M2Stats();const c=document.querySelector('canvas');return {...s,uiCanvasWidth:c?.width??0,uiCanvasHeight:c?.height??0};})()`;

export class UiPresentationError extends Error {
  constructor(code) { super(code); this.code=code; }
}

function integer(value) { return Number.isSafeInteger(value)&&value>=0; }

export function completedUiSnapshot(s, requestedRevision) {
  if(typeof s?.uiSubmissionValid!=='boolean')throw new UiPresentationError('UiReceiptUnavailable');
  if(!integer(requestedRevision)||!integer(s.uiRevision))throw new UiPresentationError('UiRevisionInvalid');
  if(!s.uiSubmissionValid)return false;
  if(![s.uiSubmittedRevision,s.uiSubmittedFrame,s.uiSubmittedWidth,s.uiSubmittedHeight,
    s.uiUploadedRevision,s.completedFrames,s.uiCanvasWidth,s.uiCanvasHeight].every(integer)
    ||s.uiSubmittedFrame===0)throw new UiPresentationError('UiReceiptInvalid');
  return s.uiSubmittedRevision>=requestedRevision
    &&s.uiSubmittedRevision>=s.uiRevision
    &&s.uiSubmittedRevision===s.uiUploadedRevision
    &&!s.uiFramePending
    &&s.uiCanvasWidth>0&&s.uiCanvasHeight>0
    &&s.uiSubmittedWidth===s.uiCanvasWidth&&s.uiSubmittedHeight===s.uiCanvasHeight
    &&s.completedFrames>=s.uiSubmittedFrame;
}

export async function waitForUiPresentation({read,pause,deadline=Infinity,now=Date.now,onPoll=()=>{}}) {
  const until=Math.min(now()+15000,deadline);
  let requestedRevision;
  while(now()<until) {
    const s=await read();
    if(now()>until)break; // A late callback never earns another grace period.
    requestedRevision??=s.uiRevision;
    const complete=completedUiSnapshot(s,requestedRevision);
    onPoll({requestedRevision,complete,snapshot:s});
    if(complete)return s;
    const left=until-now();
    if(left>0)await pause(Math.min(50,left));
  }
  throw new UiPresentationError('UiPresentationDeadline');
}
