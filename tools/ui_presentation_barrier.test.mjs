import test from 'node:test';
import assert from 'node:assert/strict';
import {completedUiSnapshot,waitForUiPresentation} from './ui_presentation_barrier.mjs';

function frame(fields={}) {
  return {uiRevision:7,uiUploadedRevision:7,uiFramePending:false,uiSubmissionValid:true,
    uiSubmittedRevision:7,uiSubmittedFrame:200,uiSubmittedWidth:780,uiSubmittedHeight:437,
    uiCanvasWidth:780,uiCanvasHeight:437,completedFrames:200,submittedFrames:201,...fields};
}

test('a completed matching UI does not wait for a newer heavy Scene',()=>{
  const s=frame();
  assert(s.completedFrames<s.submittedFrames);
  assert.equal(completedUiSnapshot(s,7),true);
});

test('uploaded or abandoned UI and stale completed revisions cannot qualify',()=>{
  assert.equal(completedUiSnapshot(frame({uiRevision:8,uiUploadedRevision:8}),8),false);
  assert.equal(completedUiSnapshot(frame({uiFramePending:true}),7),false);
  assert.equal(completedUiSnapshot(frame({completedFrames:199}),7),false);
  assert.equal(completedUiSnapshot(frame({uiSubmissionValid:false,completedFrames:900}),7),false);
});

test('resize and reprepare require the new current image rather than an old callback',async()=>{
  assert.equal(completedUiSnapshot(frame({uiCanvasWidth:960,uiCanvasHeight:540}),7),false);
  const states=[frame({completedFrames:199}),frame({uiSubmissionValid:false}),
    frame({uiSubmittedFrame:201}),frame({uiSubmittedFrame:201,completedFrames:201})];
  let at=0,time=0;
  const result=await waitForUiPresentation({read:async()=>states[at++],pause:async ms=>{time+=ms;},now:()=>time});
  assert.equal(at,4);assert.equal(result.uiSubmittedFrame,201);
});

test('coalesced revisions certify the latest actual UI and keep dimensions exact',()=>{
  assert.equal(completedUiSnapshot(frame({uiRevision:9,uiUploadedRevision:9}),7),false);
  assert.equal(completedUiSnapshot(frame({uiRevision:9,uiUploadedRevision:9,uiSubmittedRevision:9}),7),true);
  assert.equal(completedUiSnapshot(frame({uiSubmittedWidth:960}),7),false);
  assert.equal(completedUiSnapshot(frame(),8),false);
});

test('UI upload and GPU completion share a single fifteen-second deadline',async()=>{
  let time=0;
  await assert.rejects(waitForUiPresentation({now:()=>time,pause:async ms=>{time+=ms;},
    read:async()=>time<14000?frame({uiSubmissionValid:false}):frame({completedFrames:199})}),
    e=>e.code==='UiPresentationDeadline');
  assert.equal(time,15000);
});

test('outer deadline and late positive results cannot extend the proof',async()=>{
  let time=117000;
  await assert.rejects(waitForUiPresentation({now:()=>time,deadline:120000,
    pause:async ms=>{time+=ms;},read:async()=>frame({uiSubmissionValid:false})}),e=>e.code==='UiPresentationDeadline');
  assert.equal(time,120000);
  time=0;
  await assert.rejects(waitForUiPresentation({now:()=>time,pause:async()=>{},
    read:async()=>{time=15001;return frame();}}),e=>e.code==='UiPresentationDeadline');
});

test('old pairs or non-exact receipt numbers fail explicitly instead of falling back',()=>{
  assert.throws(()=>completedUiSnapshot({},7),e=>e.code==='UiReceiptUnavailable');
  assert.throws(()=>completedUiSnapshot(frame({uiSubmittedFrame:Number.MAX_SAFE_INTEGER+1}),7),e=>e.code==='UiReceiptInvalid');
});
