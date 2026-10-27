// Public, bounded post-failure diagnostics. The allowlists deliberately omit
// identity, addresses, positions, chat, status text, resource metadata and errors.
const browserNumbers = `submittedFrames completedFrames inFlightFrames
uiRevision uiUploadedRevision uiSubmittedFrame uiSubmittedRevision
uiSubmittedWidth uiSubmittedHeight renderWidth renderHeight renderScale sampleCount
residentPages residentQuads terrainFrames loadedPages queuedJobs runningJobs meshesPublished
horizonDesiredHeights horizonCachedHeights horizonReceivedDesiredHeights horizonWantedRefine
horizonUnavailableColumns horizonKnownGeometry horizonPlanId horizonAcceptedPlanId
horizonCandidatePlanId horizonCommittedPlanId horizonPlanGeneration
horizonRequestedErrorPixels horizonCommittedRequestedErrorPixels
horizonCommittedMissingErrorCertificates horizonCommittedSelectionLeafLimited
horizonCommittedMaximumErrorPixels horizonHeightStaged horizonProxyStaged
horizonFailedProxies horizonMissingErrorCertificates horizonMaximumErrorPixels
horizonSseRefinements horizonSelectionLeafLimited horizonPendingHeightConfirmations
horizonPendingExactColumns horizonQueuedJobs horizonRunningJobs horizonRetainedBytes
horizonContexts horizonPlansCompleted horizonPlansStale horizonPlansFailed
horizonLastPlanFailure horizonProxyResults horizonWorkerFailures
horizonHeightFront horizonProxyFront farWaterWanted farWaterRecords farWaterQueued
farWaterRunning farWaterReadyQueue farWaterStaged farWaterFront farWaterUnknownHeld
farWaterFailures farWaterFrontQuads farWaterStagedQuads propsInstances propsPendingJobs
propsPendingRecords propsWorkerCells pendingLightSourcePages lightPendingGpuPages
lightPendingCells lightPendingShadows lightOldestTicks`.split(/\s+/);
const browserBooleans = `connected playerSpawned gameInputAdmitted gameUiPresentationPending
uiSubmissionValid uiNeedsUpload uiFramePending cpuSceneReady ready authoritativeLighting environmentPrepared horizonPrepared
lightReady publicEyeLightKnown horizonNearGuardReady horizonCommittedFront
horizonProjectionPresent horizonCommittedSelectionCapacityLimited horizonEnabled
horizonCapturePending horizonResultPending horizonSelectionCapacityLimited
propsPrepared propsReady`.split(/\s+/);
const uiNumbers = `players settingsApplied settingsPending settingsReloadRequired
requestedSamples requestedNear effectiveNearBlocks requestedHorizon requestedClouds
requestedSsao requestedGi requestedReflections requestedHold60 renderScale renderWidth
renderHeight`.split(/\s+/);
const screens = new Set(['Welcome','Loading','Game','Death','Inventory','Pause','Resources',
  'Settings(Graphics)','Settings(Sound)','Settings(Controls)','Settings(Game)']);

function counters(source, numbers, booleans) {
  const result = {};
  if (!source || typeof source !== 'object') return result;
  for (const key of numbers) {
    const value = source[key];
    // Keep legitimate infinite SSE failure bounds explicit without JSON's
    // silent Infinity -> null conversion. Everything else must be numeric.
    if (typeof value === 'number') {
      if (Number.isFinite(value)) result[key] = value;
      else result[key] = Number.isNaN(value) ? 'NaN' : value > 0 ? 'Infinity' : '-Infinity';
    }
  }
  for (const key of booleans) if (typeof source[key] === 'boolean') result[key] = source[key];
  return result;
}

export function productTimeoutSnapshot(sample, timeoutMs) {
  const browser = counters(sample?.browser, browserNumbers, browserBooleans);
  const ui = counters(sample?.ui, uiNumbers, ['pingMeasured']);
  if (screens.has(sample?.browser?.uiScreen)) browser.uiScreen = sample.browser.uiScreen;
  if (screens.has(sample?.ui?.screen)) ui.screen = sample.ui.screen;
  if (['Owner','Admin','Player'].includes(sample?.ui?.role)) ui.role = sample.ui.role;
  return {
    schema: 1,
    acceptance: false,
    phase: 'product-ui-wait-timeout',
    timeoutMs: Number.isSafeInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 120000 ? timeoutMs : null,
    browser,
    ui,
  };
}
