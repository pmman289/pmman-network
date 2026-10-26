<template>
  <el-result icon="warning" title="404" sub-title="当前页面不存在，请检查地址或返回控制台。">
    <template #extra
      ><el-button type="primary" @click="$router.replace('/overview')"
        >返回概览</el-button
      ></template
    >
  </el-result>
</template>
