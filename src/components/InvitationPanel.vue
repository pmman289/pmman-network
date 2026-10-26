<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { grantSummary, invitationStates, type InvitationRequest, type InvitationResult, type InvitationPolicyView, type InvitationPage, type Invitation, type InvitationRegistration, type CreatedInvitation } from '../../shared/invitations';
const props=defineProps<{busy:boolean;request:(request:InvitationRequest)=>Promise<InvitationResult>}>();
const policy=ref<InvitationPolicyView>();const codes=ref<InvitationPage<Invitation>>();const records=ref<InvitationPage<InvitationRegistration>>();
const tab=ref<'codes'|'records'>('codes');const page=ref(1);const loading=ref(false);const error=ref('');const feedback=ref('');const created=ref<(CreatedInvitation&{registration_url?:string})|null>(null);
const revokeId=ref<string|null>(null);let alive=true;
onUnmounted(()=>{alive=false;created.value=null;});
const format=(value:string|null)=>value?new Date(value).toLocaleString():'长期有效';
async function load(){
  if(loading.value||props.busy)return;loading.value=true;error.value='';
  try{const p=await props.request({operation:'policy'});if(!alive)return;if(p.type==='policy')policy.value=p.data;
    const result=await props.request({operation:tab.value==='codes'?'list':'registrations',query:{page:page.value,page_size:20}});if(!alive)return;
    if(result.type==='list')codes.value=result.data;else if(result.type==='registrations')records.value=result.data;
  }catch(e){if(alive)error.value=e instanceof Error?e.message:String(e);}finally{if(alive)loading.value=false;}
}
async function changeTab(value:'codes'|'records'){if(props.busy||loading.value)return;tab.value=value;page.value=1;await load();}
async function changePage(offset:number){page.value+=offset;await load();}
async function generate(){if(props.busy||loading.value)return;error.value='';try{const result=await props.request({operation:'create'});if(!alive)return;if(result.type==='created')created.value=result.data;await load();}catch(e){if(alive)error.value=e instanceof Error?e.message:String(e);}}
async function revoke(){if(!revokeId.value||props.busy)return;try{await props.request({operation:'revoke',id:revokeId.value});if(!alive)return;revokeId.value=null;await load();}catch(e){if(alive)error.value=e instanceof Error?e.message:String(e);}}
async function copy(value:string){try{await navigator.clipboard.writeText(value);feedback.value='已复制';}catch{feedback.value='复制失败，请手动选择文本复制';}}
onMounted(()=>void load());
</script>
<template><section class="invitation-panel" aria-label="邀请管理">
  <p v-if="error" class="inline-notice error" role="alert">{{error}}</p>
  <template v-if="policy"><div class="invitation-quota"><strong>{{policy.policy.enabled?'邀请已启用':'尚未获得邀请权限'}}</strong><span>未使用 {{policy.unused}} / {{policy.policy.max_unused}}</span><span>今日 {{policy.generated_today}} / {{policy.policy.daily_limit}}</span></div>
    <p class="muted">邀请码 {{policy.policy.valid_days}} 天有效；每日额度于 {{format(policy.resets_at)}} 重置。</p><p>受邀权限：{{grantSummary(policy.policy.grant)}}</p>
  </template>
  <div class="invitation-actions"><button class="button secondary" :aria-pressed="tab==='codes'" :disabled="busy||loading" @click="changeTab('codes')">邀请码</button><button class="button secondary" :aria-pressed="tab==='records'" :disabled="busy||loading" @click="changeTab('records')">邀请记录</button><button class="button secondary" :disabled="busy||loading" @click="load">刷新</button><button class="button primary" :disabled="busy||loading||!policy?.policy.enabled||!policy?.remaining_today||!policy?.remaining_unused" @click="generate">生成邀请码</button></div>
  <div v-if="created" class="invitation-created"><strong>邀请码已生成，仅显示一次</strong><label for="created-invitation">邀请码</label><input id="created-invitation" :value="created.code" readonly /><button class="button secondary" @click="copy(created.code)">复制邀请码</button><template v-if="created.registration_url"><label for="created-link">注册链接</label><input id="created-link" :value="created.registration_url" readonly /><button class="button secondary" @click="copy(created.registration_url)">复制链接</button></template><button class="button secondary" @click="created=null">已保存，关闭</button><span role="status">{{feedback}}</span></div>
  <p v-if="loading" role="status">正在读取…</p>
  <ul v-else-if="tab==='codes'" class="invitation-list"><li v-for="item in codes?.items" :key="item.id"><div><strong>{{item.prefix}}…</strong><span>{{invitationStates[item.state]}}</span></div><p>{{format(item.created_at)}} · 到期 {{format(item.expires_at)}}</p><button v-if="item.source==='user'&&!['used','revoked'].includes(item.state)" class="button secondary" :disabled="busy" @click="revokeId=item.id">撤销</button></li><li v-if="codes&&!codes.items.length">暂无邀请码</li></ul>
  <ul v-else class="invitation-list"><li v-for="item in records?.items" :key="item.id"><div><strong>{{item.username}}</strong><span>{{format(item.created_at)}}</span></div><p>{{item.prefix}}… · {{grantSummary(item.grant)}}</p></li><li v-if="records&&!records.items.length">暂无邀请记录</li></ul>
  <div v-if="revokeId" class="inline-notice" role="alert"><p>撤销后不可恢复，已注册用户不受影响。</p><button class="button secondary" :disabled="busy" @click="revokeId=null">取消</button><button class="button danger" :disabled="busy" @click="revoke">确认撤销</button></div>
  <div class="invitation-actions"><button class="button secondary" :disabled="page<=1||busy||loading" @click="changePage(-1)">上一页</button><span>第 {{page}} 页 · {{tab==='codes'?codes?.total??0:records?.total??0}} 条</span><button class="button secondary" :disabled="busy||loading||page*20>=(tab==='codes'?codes?.total??0:records?.total??0)" @click="changePage(1)">下一页</button></div>
</section></template>
<style scoped>.invitation-panel{display:grid;gap:14px}.invitation-quota,.invitation-actions{display:flex;align-items:center;flex-wrap:wrap;gap:12px}.invitation-list{padding:0;list-style:none;margin:0;display:grid;gap:12px}.invitation-list li,.invitation-created{border:1px solid var(--border);border-radius:10px;padding:14px}.invitation-list li>div{display:flex;flex-wrap:wrap;justify-content:space-between;gap:12px}.invitation-list p{color:var(--muted);overflow-wrap:anywhere}.invitation-created{display:grid;gap:10px}.invitation-created input{width:100%;box-sizing:border-box;padding:10px;border:1px solid var(--border);background:var(--surface);color:inherit;border-radius:8px}.invitation-panel p{margin:0;line-height:1.7}</style>
