'use strict';
const $=id=>document.getElementById(id);
const fragment=new URLSearchParams(location.hash.slice(1));history.replaceState(null,'',location.pathname);
const token=fragment.get('invite')||fragment.get('reset');
async function api(path,body,method){const r=await fetch('/relay/v1/'+path,{method:method||(body?'POST':'GET'),headers:body?{'Content-Type':'application/json'}:{},body:body?JSON.stringify(body):undefined});if(!r.ok)throw Error('Request failed. Check your details or request a new link.');return r.status===204?null:r.json();}
function message(e){$('status').textContent=e.message||String(e);}
function form(id,action){$(id).addEventListener('submit',async e=>{e.preventDefault();const button=e.currentTarget.querySelector('button');button.disabled=true;try{await action(Object.fromEntries(new FormData(e.currentTarget)));e.target.reset();}catch(err){message(err)}finally{button.disabled=false}});}
async function refresh(){try{const a=await api('account');$('signed-in').hidden=false;$('signed-out').hidden=true;$('account').textContent=a.username;const data=await api('gateways');$('gateways').replaceChildren();for(const g of data.gateways){const li=document.createElement('li'),label=document.createElement('span'),open=document.createElement('button'),forget=document.createElement('button');label.textContent=g.name+(g.online?' · Online':' · Offline');open.textContent='Open';open.disabled=!g.online;open.onclick=async()=>{try{const result=await api('gateways/'+g.gatewayId+'/open',{});location.assign(result.url)}catch(e){message(e)}};forget.textContent='Forget';forget.onclick=async()=>{if(confirm('Disconnect and forget '+g.name+'?'))try{await api('gateways/'+g.gatewayId,null,'DELETE');await refresh()}catch(e){message(e)}};li.append(label,open,forget);$('gateways').append(li)}}catch{$('signed-in').hidden=true;$('signed-out').hidden=false;}}
form('login',async data=>{await api('login',data);await refresh();message('Signed in.');});
form('set-password',async data=>{await api(fragment.has('reset')?'reset':'register',{token,password:data.password});$('redeem').hidden=true;message('Password saved. Sign in to continue.');});
form('change',async data=>{await api('password',data);await refresh();message('Password changed. All browser sessions were signed out.');});
form('bind',async data=>{data.userCode=data.userCode.trim().toUpperCase();const preview=await api('bindings/preview',data);if(confirm('Bind “'+preview.name+'” to your account? This grants your account full access to this workbench.')){await api('bindings/approve',data);message('Workbench bound.');await refresh()}});
$('logout').onclick=async()=>{try{await api('logout',{});await refresh()}catch(e){message(e)}};$('refresh').onclick=refresh;
if(token){$('redeem').hidden=false;$('redeem-title').textContent=fragment.has('reset')?'Reset your password':'Accept invitation';}
if(fragment.has('bind'))$('bind').elements.userCode.value=fragment.get('bind');
refresh();
