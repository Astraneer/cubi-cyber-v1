import assert from 'node:assert/strict';
import test from 'node:test';
import {productTimeoutSnapshot} from './product_ui_diagnostics.mjs';

test('Loading failure retains the counters that distinguish SSE, leases, jobs and GPU progress', () => {
  const snapshot = productTimeoutSnapshot({browser: {
    uiScreen:'Loading', connected:true, playerSpawned:true,
    horizonReceivedDesiredHeights:265, horizonKnownGeometry:512,
    horizonPendingExactColumns:22, horizonSelectionLeafLimited:22,
    horizonMaximumErrorPixels:50.2896, horizonSelectionCapacityLimited:false,
    horizonQueuedJobs:0, horizonPlansCompleted:46, horizonPlansStale:46,
    submittedFrames:6055, completedFrames:6055, gameInputAdmitted:false,
  }, ui:{screen:'Loading', role:'Player', settingsPending:4103, pingMeasured:true}},120000);
  assert.equal(snapshot.acceptance,false);
  assert.equal(snapshot.timeoutMs,120000);
  assert.equal(snapshot.browser.horizonPendingExactColumns,22);
  assert.equal(snapshot.browser.horizonMaximumErrorPixels,50.2896);
  assert.equal(snapshot.browser.horizonKnownGeometry,512);
  assert.equal(snapshot.browser.submittedFrames,snapshot.browser.completedFrames);
  assert.equal(snapshot.browser.horizonSelectionCapacityLimited,false);
  assert.deepEqual(snapshot.ui,{settingsPending:4103,pingMeasured:true,screen:'Loading',role:'Player'});
});

test('private payloads and all free-form strings stay outside the fixed projection', () => {
  const privateText = 'PRIVATE_CANARY_NOT_FOR_ARTIFACTS';
  const payload = {
    identity:privateText, token:privateText, address:privateText, lastChat:privateText,
    uiStatus:privateText, resourceName:privateText, playerPosition:[privateText],
    horizonKnownGeometry:{privateText}, horizonPlanId:privateText,
    connected:privateText, uiScreen:privateText, screen:privateText, role:privateText,
  };
  const snapshot = productTimeoutSnapshot({browser:payload,ui:payload},privateText);
  assert(!JSON.stringify(snapshot).includes(privateText));
  assert.deepEqual(snapshot.browser,{});
  assert.deepEqual(snapshot.ui,{});
  assert.equal(snapshot.timeoutMs,null);
});

test('nonfinite SSE values survive JSON with explicit diagnostic tags', () => {
  const snapshot=productTimeoutSnapshot({browser:{
    horizonMaximumErrorPixels:Infinity,
    horizonCommittedMaximumErrorPixels:NaN,
    horizonRequestedErrorPixels:-Infinity,
  }},120000);
  assert.equal(snapshot.browser.horizonMaximumErrorPixels,'Infinity');
  assert.equal(snapshot.browser.horizonCommittedMaximumErrorPixels,'NaN');
  assert.equal(snapshot.browser.horizonRequestedErrorPixels,'-Infinity');
  assert.deepEqual(JSON.parse(JSON.stringify(snapshot)),snapshot);
});

test('missing public snapshots and oversized unrelated data cannot enlarge the artifact', () => {
  assert.deepEqual(productTimeoutSnapshot(undefined,120000).browser,{});
  const payload={uiStatus:'x'.repeat(1_000_000),lastChat:'x'.repeat(1_000_000)};
  const snapshot=productTimeoutSnapshot({browser:payload,ui:payload},120000);
  assert(JSON.stringify(snapshot).length<256);
});
