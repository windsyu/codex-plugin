import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { CleanupPreviewPanel, HistoryStorage, sizeLabel } from './HistoryStorage';
const root=document.createElement('div');
beforeEach(()=>document.body.append(root));
afterEach(()=>{render(null,root);root.remove();vi.unstubAllGlobals();vi.useRealTimers();});
const success=(result:unknown)=>({ok:true,json:async()=>({currentRunEpoch:'current',result})});
it('does not turn missing or partial size into zero',()=>{
  expect(sizeLabel()).toBe('统计中');
  expect(sizeLabel({bytes:null,knownBytes:0,status:'partial',measuredAt:''})).toBe('占用无法完整统计');
  expect(sizeLabel({bytes:null,knownBytes:2048,status:'partial',measuredAt:''})).toContain('至少 2.0 KiB');
});
it('refreshes visible usage after retention policy changes without repeatedly reloading history',async()=>{
  vi.useFakeTimers();
  const size={bytes:0,knownBytes:0,status:'complete',measuredAt:'2026-01-01'};
  const usage={scanId:'scan',state:'complete',runCount:1,historyBytes:128,activeBytes:0,unknownRuns:0,unverifiedEntries:0,examinedEntries:1,measuredAt:'2026-01-01',pendingCleanup:size,sharedManagement:size,retention:{enabled:false,days:1,state:'disabled',lastCheckAt:null,nextCheckAt:null,skippedCounts:{}}};
  let reads=0;
  const fetch=vi.fn(async (url:string)=>success(url.endsWith('/refresh')?{scheduled:true}:{...usage,retention:{...usage.retention,enabled:++reads>1,state:'waiting'}}));
  vi.stubGlobal('fetch',fetch);const measured=vi.fn();
  await act(async()=>{render(<HistoryStorage epoch="current" active onMeasured={measured}/>,root);});
  await act(async()=>{await vi.advanceTimersByTimeAsync(150);});
  expect(root.textContent).not.toContain('自动保留');
  await act(async()=>{await vi.advanceTimersByTimeAsync(5000);});
  expect(root.textContent).toContain('自动保留 1 天');
  expect(measured).toHaveBeenCalledOnce();
  await act(async()=>{render(<HistoryStorage epoch="current" active={false} onMeasured={measured}/>,root);});
  const count=fetch.mock.calls.length;await act(async()=>{await vi.advanceTimersByTimeAsync(10000);});expect(fetch).toHaveBeenCalledTimes(count);
});
it('refresh within settings never submits the settings form and inactive panels do not scan',async()=>{
  const fetch=vi.fn().mockResolvedValue(success({scheduled:true}));vi.stubGlobal('fetch',fetch);const submit=vi.fn();
  await act(async()=>{render(<form onSubmit={e=>{e.preventDefault();submit();}}><HistoryStorage epoch="current" active={false}/></form>,root);});
  expect(fetch).not.toHaveBeenCalled();root.querySelector('button')!.click();expect(submit).not.toHaveBeenCalled();
});
it('keeps incomplete coverage visible even with storage details collapsed',async()=>{
  vi.useFakeTimers();
  const size={bytes:0,knownBytes:0,status:'complete',measuredAt:'2026-01-01'};
  vi.stubGlobal('fetch',vi.fn(async(url:string)=>success(url.endsWith('/refresh')?{scheduled:true}:{scanId:'partial',state:'complete',runCount:2,historyBytes:128,activeBytes:0,unknownRuns:1,unverifiedEntries:0,measuredAt:'2026-01-01',pendingCleanup:size,sharedManagement:size})));
  await act(async()=>render(<HistoryStorage epoch="current" active/>,root));
  await act(async()=>{await vi.advanceTimersByTimeAsync(150);});
  expect(root.querySelector('.wb-storage-details')?.hasAttribute('open')).toBe(false);
  expect(root.querySelector('.wb-history-storage-summary')?.textContent).toContain('部分统计');
  expect(root.querySelector('.wb-history-storage-summary')?.textContent).toContain('历史已统计');
});
it('freezes explicit preview IDs, renders skips, escapes data and never sends a deletion',async()=>{
  const fetch=vi.fn().mockResolvedValueOnce(success({previewId:'preview'})).mockResolvedValueOnce(success({previewId:'preview',status:'ready',configRevision:'rev',expiresAt:'',mode:'manual',executable:false,items:[{runEpoch:'<svg onload=alert(1)>',eligible:false,reason:'active_run',run:null}],scanComplete:true,error:null,skippedCounts:{}}));vi.stubGlobal('fetch',fetch);
  const selected=['run-1'];await act(async()=>{render(<CleanupPreviewPanel epoch="current" selection={selected} onClose={()=>{}}/>,root);});await act(async()=>{await new Promise(resolve=>setTimeout(resolve,0));});
  expect(root.querySelector('svg')).toBeNull();expect(root.textContent).toContain('仍有工作台在写入');expect(root.textContent).toContain('历史删除尚未开启');
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({currentRunEpoch:'current',runEpochs:['run-1']});
  expect(fetch.mock.calls.some(c=>String(c[0]).includes('/jobs'))).toBe(false);
});
it('does not report an unknown deletion size as zero in the confirmation',async()=>{
  const preview={previewId:'preview',status:'ready',configRevision:'rev',mode:'manual',executable:true,items:[{runEpoch:'old',eligible:true,run:{state:'ended',startedAt:'2026-01-01',size:{bytes:null,knownBytes:0}}}],scanComplete:true,skippedCounts:{}};
  vi.stubGlobal('fetch',vi.fn().mockResolvedValueOnce(success({previewId:'preview'})).mockResolvedValueOnce(success(preview)));
  const selected=['old'];await act(async()=>render(<CleanupPreviewPanel epoch="current" selection={selected} onClose={()=>{}}/>,root));
  await vi.waitFor(()=>expect(root.querySelector('.wb-cleanup-summary')?.textContent).toContain('占用暂无法完整统计'));
  expect(root.querySelector('.wb-cleanup-summary')?.textContent).not.toContain('0 B');
});
it.each([false,true])('only confirms once and reads the same operation receipt when the create response is lost: %s',async(lost)=>{
  vi.stubGlobal('crypto',{randomUUID:()=> '11111111-1111-4111-8111-111111111111'});
  const p={previewId:'preview',status:'ready',configRevision:'revision',expiresAt:'',mode:'manual',executable:true,items:[{runEpoch:'run-1',eligible:true,reason:null,run:{state:'ended',startedAt:'2026-01-01',size:{bytes:128}}}],scanComplete:true,error:null,skippedCounts:{}};
  const result={jobId:'11111111-1111-4111-8111-111111111111',status:'complete',items:[{runEpoch:'run-1',state:'deleted',reason:null,pendingCleanup:false}]};
  const fetch=vi.fn().mockResolvedValueOnce(success({previewId:'preview'})).mockResolvedValueOnce(success(p)).mockImplementationOnce(async()=>{if(lost)throw new Error('连接中断');return success(result);}).mockResolvedValueOnce(success(result));vi.stubGlobal('fetch',fetch);
  const changed=vi.fn();const selected=['run-1'];await act(async()=>{render(<CleanupPreviewPanel epoch="current" selection={selected} onClose={()=>{}} onChanged={changed}/>,root);});await act(async()=>{await new Promise(resolve=>setTimeout(resolve,0));});
  const button=[...root.querySelectorAll('button')].find(b=>b.textContent!.includes('永久删除'))!;
  await act(async()=>{button.click();button.click();await new Promise(resolve=>setTimeout(resolve,0));});await act(async()=>{await new Promise(resolve=>setTimeout(resolve,0));});
  const creates=fetch.mock.calls.filter(c=>c[0]==='/workbench/v1/history/cleanup/jobs'&&c[1].method==='POST');expect(creates).toHaveLength(1);
  expect(JSON.parse(creates[0][1].body).operationId).toBe('11111111-1111-4111-8111-111111111111');expect(root.textContent).toContain('已删除');expect(changed).toHaveBeenCalledOnce();
  if(lost)expect(root.textContent).toContain('不会自动重发删除');
});
