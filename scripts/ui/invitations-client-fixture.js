// Browser-only invitation UI fixture. Not imported or shipped by the app.
// Backend transaction and authorization acceptance uses real PostgreSQL separately.
(() => {
  if (!['localhost', '127.0.0.1'].includes(location.hostname)) return;
  const grant = { allow_ipv4_global:false,allow_ipv6_global:false,allow_ipv4_split:true,allow_ipv6_split:false,
    port_forward:{enabled:false,max_active_forwards:0,allowed_entry_ids:[],allowed_protocols:['tcp'],allowed_families:['ipv4','ipv6'],allowed_target_ports:null} };
  const codes=[];const records=[];let loggedIn=false;
  const controls={failNext:'',calls:[]};window.__PMMAN_INVITATION_FIXTURE__=controls;
  const snapshot=()=>({
    clientUpdate:{releaseNotes:[],currentVersion:'ui-fixture',availableVersion:null,state:'current',progress:0,message:'',ready:false,installerPath:null,sha256:null,mandatory:false},
    authRestorePending:false,p2pEnabled:true,p2pMessage:'',p2pPeers:[],portForwardEntries:[],portForwardQuota:{enabled:false,limit:0,used:0,available:0},portForwards:[],portForwardMessage:'',
    diagnosticUploadState:'idle',diagnosticUploadMessage:'',diagnosticUploadId:null,state:'disconnected',authenticated:loggedIn,userId:loggedIn?'ui-user':null,username:loggedIn?'邀请测试用户':null,role:loggedIn?'user':null,
    message:loggedIn?'已登录':'未登录',nodes:[],nodeSelection:'auto',ipv4Mode:'internal',ipv6Mode:'internal',allowIpv4Global:false,allowIpv6Global:false,allowIpv4Split:true,allowIpv6Split:false,
    allowedIps:[],nodeDnsServers:[],dnsServers:[],dnsSplitEnabled:false,nodeRateLimit:{uploadMbps:0,downloadMbps:0},
    trafficStats:{nodeTxBytes:0,nodeRxBytes:0,p2pDirectTxBytes:0,p2pDirectRxBytes:0,p2pRelayTxBytes:0,p2pRelayRxBytes:0},splitIpv4PrefixCount:0,splitIpv6PrefixCount:0,splitRouteInstallMs:0,
  });
  window.isTauri=true;
  window.__TAURI_INTERNALS__={async invoke(command,args={}) {
    controls.calls.push({command,operation:args.request?.operation});
    await new Promise(resolve=>setTimeout(resolve,80));
    if(command==='login'){loggedIn=true;return snapshot();}
    if(command==='logout'){loggedIn=false;return snapshot();}
    if(command!=='invitation_request')return snapshot();
    const request=args.request;
    if(controls.failNext===request.operation){controls.failNext='';throw new Error('测试请求失败，请重试');}
    if(request.operation==='register')return {type:'registered',data:{username:request.username}};
    if(request.operation==='policy')return {type:'policy',data:{account_enabled:true,policy:{enabled:true,max_unused:10,daily_limit:20,valid_days:7,grant},revision:1,unused:codes.filter(c=>c.state==='available').length,generated_today:codes.length,remaining_unused:10-codes.filter(c=>c.state==='available').length,remaining_today:20-codes.length,resets_at:new Date(Date.now()+86400000).toISOString()}};
    if(request.operation==='list')return {type:'list',data:{items:codes,total:codes.length,page:1,page_size:20}};
    if(request.operation==='registrations')return {type:'registrations',data:{items:records,total:records.length,page:1,page_size:20}};
    if(request.operation==='create'){
      const invitation={id:crypto.randomUUID(),prefix:'fixture',inviter_username:'ui-user',creator_username:'ui-user',source:'user',kind:'single',state:'available',note:'',created_at:new Date().toISOString(),expires_at:new Date(Date.now()+7*86400000).toISOString(),use_count:0,grant,policy_revision:1};codes.unshift(invitation);
      return {type:'created',data:{invitation,code:'f'.repeat(64),registration_path:'/register#invite='+ 'f'.repeat(64),registration_url:location.origin+'/register#invite='+ 'f'.repeat(64)}};
    }
    if(request.operation==='revoke'){codes.find(c=>c.id===request.id).state='revoked';return {type:'revoked'};}
    throw new Error('未实现的测试请求');
  }};
})();
