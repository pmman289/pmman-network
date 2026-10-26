<script setup lang="ts">
import { ref } from 'vue';
import type { InvitationRequest, InvitationResult } from '../../shared/invitations';
const props=defineProps<{busy:boolean;request:(request:InvitationRequest)=>Promise<InvitationResult>}>();
const emit=defineEmits<{registered:[username:string];cancel:[]}>();
const code=ref('');const username=ref('');const password=ref('');const confirm=ref('');const error=ref('');
async function submit(){
  if(props.busy)return;
  if(!/^[a-fA-F0-9]{64}$/.test(code.value.trim())){error.value='请输入完整邀请码';return;}
  if(!/^[A-Za-z0-9_.-]{1,64}$/.test(username.value.trim())){error.value='用户名仅支持字母、数字、点、短横线和下划线，最长 64 位';return;}
  if(password.value.length<6||password.value.length>256){error.value='密码长度为 6 至 256 位';return;}
  if(password.value!==confirm.value){error.value='两次输入的密码不一致';return;}
  error.value='';
  try{const result=await props.request({operation:'register',code:code.value.trim(),username:username.value.trim(),password:password.value});
    if(result.type!=='registered')throw new Error('注册响应无效');
    password.value='';confirm.value='';code.value='';emit('registered',result.data.username);
  }catch(e){error.value=e instanceof Error?e.message:String(e);}
}
</script>
<template><form class="login-form" @submit.prevent="submit">
  <label for="register-code">邀请码</label><input id="register-code" v-model="code" autocomplete="off" maxlength="64" required :disabled="busy" />
  <label for="register-username">用户名</label><input id="register-username" v-model="username" autocomplete="username" maxlength="64" required :disabled="busy" />
  <label for="register-password">密码</label><input id="register-password" v-model="password" type="password" autocomplete="new-password" minlength="6" maxlength="256" required :disabled="busy" />
  <label for="register-confirm">确认密码</label><input id="register-confirm" v-model="confirm" type="password" autocomplete="new-password" minlength="6" maxlength="256" required :disabled="busy" />
  <p v-if="error" class="inline-notice error" role="alert">{{error}}</p>
  <button class="button primary login-button" type="submit" :disabled="busy">{{busy?'正在注册…':'注册'}}</button>
  <button class="button secondary" type="button" :disabled="busy" @click="emit('cancel')">返回登录</button>
</form></template>
