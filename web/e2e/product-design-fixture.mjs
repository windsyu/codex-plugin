// Synthetic visual fixture only: no CLI, disk history, account or external service.
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
const require=createRequire(import.meta.url);
const {WebSocketServer}=require('ws');
export const runId='11111111-1111-4111-8111-111111111111';
export const savedId='22222222-2222-4222-8222-222222222222';
export const instanceId='33333333-3333-4333-8333-333333333333';
const meta={readAt:'2026-11-17T00:00:00Z',elapsedMs:2,truncated:false};
const key={requestId:'synthetic-request',responseId:'synthetic-response',wireItemId:'synthetic-model',contentIndex:0};
export function snapshot(epoch=runId){return {
  schemaVersion:2,runEpoch:epoch,viewSeq:2,capture:'observed',toolContexts:[],nativeCommands:[],nativeFileChanges:[],diagnostics:[],userCapture:{enabled:true,diagnostics:[]},
  requests:[{requestId:key.requestId,responseId:key.responseId,clientRequestIndex:null,requestedModel:'synthetic-model',codexThreadId:'synthetic-thread',codexTurnId:'synthetic-turn',purpose:'conversation',purposeBasis:'codex_turn_metadata'}],
  responses:[{requestId:key.requestId,responseId:key.responseId,status:'receiving',reportedModels:['synthetic-model']}],
  items:[{itemKey:'synthetic-user',kind:'message',revision:1,orderIndex:0,completeness:'observed',truncated:false,author:{role:'user'},streamState:'ended',omitted:false,content:[{contentKey:'user-text',text:'请核对商业界面的连续阅读、文件与终端布局。此内容完全合成。'}],evidence:[{origin:'rollout',key:{codexThreadId:'synthetic-thread',codexTurnId:'synthetic-turn',nativeItemId:'synthetic-user'},source:{sourceRef:'synthetic-rollout',byteOffset:0,ordinal:0}}]},
  {itemKey:'synthetic-model',kind:'message',revision:1,orderIndex:1,completeness:'observed',truncated:false,author:{role:'assistant',requestedModel:'synthetic-model',reportedModels:['synthetic-model']},streamState:'receiving',content:[{contentKey:'model-text',text:'## 连续阅读示例\n\n这是生产阅读组件中的合成内容，用于检查中文行高、长段落和终端并排时的信息层级。\n\n'+Array.from({length:6},(_,i)=>`### ${i+1}. 核对工作上下文\n\n电脑、项目和当前运行始终保持明确。网页只展示观察副本；命令输入和审批留在原生终端。记录状态与模型响应状态分别表达，不将捕获完成解释为任务完成。\n\n- 阅读当前项目中的文件。\n- 搜索结果携带文件路径与行号。\n- Git 更改仅供阅读，不声明已应用。`).join('\n\n')+'\n\n```ts\nexport const syntheticOnly = true;\n```'}],evidence:[{origin:'model',key,captureSeq:2}]}]
};}
// History catalog output is deliberately synthetic. Rust title extraction and
// review classification are validated separately by backend fixture tests.
export const historyProjects=[
 {projectId:'synthetic-project-a',path:'/synthetic/team-a/shared-app',entries:1},
 {projectId:'synthetic-project-b',path:'/synthetic/team-b/shared-app',entries:1},
 {projectId:'synthetic-review-only',path:'/synthetic/internal/review-only',entries:1}
];
export const historyGroups=historyProjects.map((project,index)=>{
 const auxiliary=index===2;
 const member={entryId:'synthetic-history-'+index,sourceId:'synthetic-native',kind:'native',projectBasis:'cwd_inferred',sourceRevision:'synthetic-history-source-1',title:['请修复项目中的登录跳转问题','请核对目录过滤与完整项目路径','后台审批审查：检查本次命令'][index],preview:{text:['已修复登录跳转，等待重新加载验证。','最近一次交流：目录过滤现在保留完整路径。','辅助 review：命令已完成风险核对。'][index],role:'assistant'},coverage:{state:'complete_for_source',reasons:[]},capabilities:{read:true,resume:false,inspectCalls:false,manage:false}};
 return {groupId:'synthetic-group-'+index,section:auxiliary?'auxiliary':'primary',conversationKind:auxiliary?'background_review':'ordinary',preferredEntryId:member.entryId,members:[member],projectId:project.projectId,projectPath:project.path,latestRecordedAt:'2026-11-17T00:00:00Z'};
});
export function productFixture(repo){
 const state={mode:'local',historyEnabled:false,requests:[],unexpected:[],sockets:0,input:[],sse:0,portalSignedIn:false,portalForced:false};
 const sockets=new Set(),streams=new Set();
 const ws=new WebSocketServer({noServer:true});
 const app=()=>({instanceId,runs:[{runId,projectName:'商业界面示例',projectPath:'/synthetic/projects/design-workbench',state:'running'}],sources:[{id:'synthetic-native',kind:'native',state:'ready',message:'合成历史源 · 未读取本机记录',revision:'synthetic-1'}],launchError:null,launchAvailable:true,directoryPickerAvailable:state.mode==='local',directoryBrowserAvailable:state.mode==='remote',remoteOwner:state.mode==='remote'});
 const run=()=>({runEpoch:runId,projectName:'商业界面示例',processId:null,terminalAvailable:true,workspaceRoot:'/synthetic/projects/design-workbench',historyAvailable:true,accessAvailable:state.mode!=='paired',settingsAvailable:state.mode!=='paired',historyManagementAvailable:false});
 const history={runEpoch:savedId,projectName:'合成已保存对话',startedAt:'2026-11-16T10:00:00Z',state:'ended',savedThroughViewSeq:2,persistedThroughViewSeq:2,gapCount:0,historyCoverage:'complete_for_observed_scope'};
 const json=(res,value,status=200)=>{res.writeHead(status,{'Content-Type':'application/json','Cache-Control':'no-store'});res.end(JSON.stringify(value));};
 const gateways=['online','offline','awaitingConnection','reauthenticationRequired','versionIncompatible'].map((status,i)=>({gatewayId:`aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa${i}`,name:['Design Mac','离线电脑','等待连接的电脑','需要重新认证的电脑','版本不兼容的电脑'][i],status}));
 const plugin={name:'synthetic-product-design-fixture',configureServer(server){
  server.httpServer.on('upgrade',(request,socket,head)=>{if(new URL(request.url,'http://fixture').pathname!==`/workbench/v1/runs/${runId}/terminal`)return;ws.handleUpgrade(request,socket,head,client=>ws.emit('connection',client,request));});
  server.middlewares.use((req,res,next)=>{
   const url=new URL(req.url,'http://fixture'),p=url.pathname;
   if(['/login','/computers','/authorize','/account/security','/admin/users'].includes(p)) {res.setHeader('Content-Type','text/html');res.end(fs.readFileSync(path.join(repo,'relay/static/index.html')));return;}
   if(p==='/relay.css'||p==='/relay.js'){res.setHeader('Content-Type',p.endsWith('css')?'text/css':'text/javascript');res.end(fs.readFileSync(path.join(repo,'relay/static',p.endsWith('css')?'portal.css':'portal.js')));return;}
   if(!p.startsWith('/workbench/v1/')&&!p.startsWith('/relay/v1/')){if(p==='/'||/^\/runs\/[^/]+\/[^/]+$/.test(p)||['/history','/relay','/launch','/settings'].includes(p))req.url='/workbench.html'+url.search;return next();}
   state.requests.push({method:req.method,path:p,query:url.search});
   if(p.startsWith('/relay/v1/')){
    if(p==='/relay/v1/account')return state.portalSignedIn?json(res,{accountId:'synthetic-account',username:'synthetic_user',role:'user',mustChangePassword:state.portalForced}):json(res,{error:'synthetic_signed_out'},401);
    if(p==='/relay/v1/gateways'&&state.portalSignedIn)return json(res,{gateways});
    if(p==='/relay/v1/login'&&req.method==='POST'){state.portalSignedIn=true;return json(res,{});}
    if((p==='/relay/v1/password'||p==='/relay/v1/logout')&&req.method==='POST'){state.portalSignedIn=false;state.portalForced=false;return json(res,{});}
    state.unexpected.push({method:req.method,path:p});return json(res,{error:'synthetic_not_implemented'},501);
   }
   if(req.method!=='GET'){state.unexpected.push({method:req.method,path:p});return json(res,{error:'synthetic_writes_forbidden'},501);}
   if(p==='/workbench/v1/application')return state.mode==='paired'?json(res,{error:'run_only'},404):json(res,app());
   if(p==='/workbench/v1/application/relay')return state.mode==='local'?json(res,{instanceId,relay:{state:'disabled',enabled:false,name:'Design Mac'}}):json(res,{error:'forbidden'},403);
   if(state.historyEnabled&&p.startsWith('/workbench/v1/library/')){
    // projectScope belongs exclusively to project navigation. Old API callers
    // omit it for all; the production UI explicitly asks for primary by default.
    const project=url.searchParams.get('projectId'),query=url.searchParams.get('q')||'';
    const selected=historyProjects.find(item=>item.projectId===project);
    const matches=historyGroups.filter(group=>(!project||group.projectId===project)&&(!query||group.members.some(member=>(member.title+' '+member.preview.text).includes(query))));
    const coverage={state:'complete_for_source',reasons:[]};
    const common={revision:'synthetic-history-catalog-1',nextCursor:null,coverage,unassignedRecords:0,projectExists:!project||!!selected,selectedProject:selected?{projectId:selected.projectId,path:selected.path,status:'available'}:null};
    if(p==='/workbench/v1/library/projects'){
     const projectQ=url.searchParams.get('projectQ')||'';
     const projectScope=url.searchParams.get('projectScope')||'all';
     if(!['primary','all'].includes(projectScope))return json(res,{error:'invalid_project_scope'},400);
     return json(res,{...common,records:historyProjects.filter(item=>(projectScope==='all'||historyGroups.some(group=>group.projectId===item.projectId&&group.section==='primary'))&&(!projectQ||item.path.includes(projectQ)))});
    }
    if(p==='/workbench/v1/library/session-groups'){
     const section=url.searchParams.get('section')||'primary';
     const counts=Object.fromEntries(['primary','auxiliary','runs'].map(section=>[section,{groups:matches.filter(group=>group.section===section).length,records:matches.filter(group=>group.section===section).length}]));
     return json(res,{...common,records:matches.filter(group=>group.section===section),counts,totalRecords:matches.length});
    }
    if(p==='/workbench/v1/library/entries')return json(res,{...common,records:matches.map(group=>({...group.members[0],projectId:group.projectId,projectPath:group.projectPath,recordedAt:group.latestRecordedAt,nativeThreadId:'synthetic-native-thread',runId:null,relatedEntryIds:[],isSubagent:group.section==='auxiliary'})),totalRecords:matches.length});
    const detail=historyGroups.find(group=>p==='/workbench/v1/library/entries/'+group.preferredEntryId);
    if(detail){const member=detail.members[0];return json(res,{...common,entry:{...member,projectId:detail.projectId,projectPath:detail.projectPath,recordedAt:detail.latestRecordedAt,nativeThreadId:'synthetic-native-thread',runId:null,relatedEntryIds:[]},window:2,records:[{kind:'message',role:'user',presentation:'conversation',text:member.title,detailCursor:'synthetic-record-0'},{kind:'message',role:'assistant',presentation:'conversation',text:member.preview.text,detailCursor:'synthetic-record-1'}]});}

   }
   if(p===`/workbench/v1/runs/${runId}/run`)return json(res,run());
   if(p===`/workbench/v1/runs/${runId}/live/snapshot`)return json(res,snapshot());
   if(p===`/workbench/v1/runs/${runId}/live/events`){
    state.sse++;res.writeHead(200,{'Content-Type':'text/event-stream','Cache-Control':'no-store','Connection':'keep-alive'});streams.add(res);
    res.write(': synthetic SSE observation\n\n');
    res.write('event: view\ndata: '+JSON.stringify({runEpoch:runId,kind:'item.patch',viewSeq:3,itemKey:'synthetic-model',baseRevision:1,revision:2,truncated:false,field:'text',contentKey:'model-text',append:'\n\nSSE 中间内容已到达；响应仍在接收。'})+'\n\n');
    const heartbeat=setInterval(()=>res.write(': fixture heartbeat\n\n'),15000);res.on('close',()=>{clearInterval(heartbeat);streams.delete(res);});return;
   }
   const base=`/workbench/v1/runs/${runId}`;
   if(p===base+'/access')return json(res,{runEpoch:runId,revision:1,state:'off',addresses:[],devices:[],notice:null,error:null,pairingId:'synthetic-pairing-off'});
   if(p===base+'/workspace/files')return json(res,{...meta,path:'',entries:[{name:'README.md',path:'README.md',kind:'file'},{name:'src',path:'src',kind:'directory'},{name:'disabled-link',path:'disabled-link',kind:'unavailable'}],nextCursor:null,omitted:0});
   if(p===base+'/workspace/file')return json(res,{...meta,path:url.searchParams.get('path'),text:'# 合成项目文件\n\n这里是只读文件夹具，不访问用户磁盘。\n\n```ts\nexport const designFixture = true;\n```',revision:'synthetic-file-1'});
   if(p===base+'/workspace/search')return json(res,{...meta,hits:[{path:'README.md',line:3,text:'designFixture 合成匹配',truncated:false}],scannedFiles:1,omitted:0,limitReason:null});
   if(p===base+'/workspace/git/status')return json(res,{...meta,branch:'synthetic/design',entries:[{path:'README.md',index:' ',working:'M',originalPath:null}],omitted:0});
   if(p===base+'/workspace/git/log')return json(res,{...meta,commits:[{id:'a'.repeat(40),shortId:'aaaaaaa',author:'Synthetic author',date:meta.readAt,subject:'docs: synthetic design fixture'}],nextCursor:null});
   if(p===base+'/workspace/git/diff')return json(res,{...meta,path:'README.md',patch:'diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-previous synthetic line\n+updated synthetic line\n',revision:'synthetic-diff-1'});
   if(p===base+'/history')return json(res,{currentRunEpoch:runId,runs:[history],nextCursor:null,diagnostics:[],indexState:'ready'});
   if(p===base+'/history/'+savedId)return json(res,{currentRunEpoch:runId,run:history,snapshot:snapshot(savedId),uncertainTail:false,gaps:[],issues:[],detailsPartial:false});
   state.unexpected.push({method:req.method,path:p});return json(res,{error:'synthetic_not_implemented'},501);
  });
 }};
 ws.on('connection',client=>{
  state.sockets++;sockets.add(client);let outputSeq=0;
  const send=value=>{if(client.readyState===1)client.send(JSON.stringify({runEpoch:runId,...value}));};
  send({type:'snapshot',connectionId:'synthetic-connection',outputSeq,complete:true,truncated:false,exit:null,rows:24,cols:80,screen:Buffer.from('Synthetic Codex terminal — no real CLI\r\n› ').toString('base64'),replay:'',control:{controllerConnection:null,generation:0,reconnectReserved:false,ended:false,rows:24,cols:80}});
  client.on('message',bytes=>{const frame=JSON.parse(bytes.toString());if(frame.runEpoch!==runId){state.unexpected.push({terminal:'wrong_run'});return client.close();}
   if(frame.command.type==='claim')return send({type:'grant',id:frame.id,grant:{generation:1,reconnectSecret:'synthetic-reconnect-only'}});
   if(frame.command.type==='resize')send({type:'state',exit:null,control:{controllerConnection:'synthetic-connection',generation:1,reconnectReserved:false,ended:false,rows:frame.command.rows,cols:frame.command.cols}});
   if(frame.command.type==='input'){const text=Buffer.from(frame.command.data,'base64').toString();state.input.push(text);send({type:'output',outputSeq:++outputSeq,data:Buffer.from(text).toString('base64')});}
   if(!['input','resize','claim'].includes(frame.command.type))state.unexpected.push({terminal:frame.command.type});
   send({type:'ack',id:frame.id});
  });client.on('close',()=>sockets.delete(client));
 });
 return {plugin,state,async close(){for(const stream of streams)stream.end();for(const socket of sockets)socket.terminate();await new Promise(resolve=>ws.close(resolve));}};
}
