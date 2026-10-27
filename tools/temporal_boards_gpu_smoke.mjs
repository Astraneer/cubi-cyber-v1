import {analyzeTemporalBoards,assertTemporalBoards} from './temporal_boards_reference.mjs';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
import {withBrowser} from './browser.mjs';
const folder=resolve('.local/artifacts/temporal-boards-probe');await mkdir(folder,{recursive:true});
let value;
if(process.env.CUBI_TEMPORAL_VIEW_NATIVE)value=JSON.parse(await readFile(process.env.CUBI_TEMPORAL_VIEW_NATIVE,'utf8'));
else{
  process.env.CUBI_WEB_DIST??=resolve('.local/taa-view-web');
  value=await withBrowser({path:'/',width:128,height:96},async({call,waitFor})=>{
    await waitFor('document.querySelector("canvas")!==null',Boolean,10_000);
    const result=await call('Runtime.evaluate',{expression:`(async()=>{const m=await import('/pkg/temporal_view_probe.js');await m.default();return await m.run_temporal_boards_probe(document.querySelector('canvas'));})()`,returnByValue:true,awaitPromise:true},120_000);
    if(result.exceptionDetails)throw Error(JSON.stringify(result.exceptionDetails));
    return JSON.parse(result.result.value);
  });
}
const kind=process.env.CUBI_TEMPORAL_VIEW_NATIVE?'native':'dawn';
await writeFile(resolve(folder,`${kind}-raw.json`),JSON.stringify(value));
const result=analyzeTemporalBoards(value);
await writeFile(resolve(folder,`${kind}-analysis.json`),JSON.stringify(result,null,2)+'\n');
console.log('ACTUAL_BOARDS_VIEW_TAA_ANALYSIS '+JSON.stringify(result));
assertTemporalBoards(result,{requireCanvas:!process.env.CUBI_TEMPORAL_VIEW_NATIVE});
