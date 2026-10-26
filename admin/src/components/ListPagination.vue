<script setup lang="ts">
defineProps<{ total: number }>();
const page = defineModel<number>('page', { required: true });
const size = defineModel<number>('size', { required: true });
</script>
<template>
  <div class="pagination">
    <el-pagination
      v-model:current-page="page"
      v-model:page-size="size"
      :page-sizes="[10, 20, 50]"
      :total="total"
      layout="total, sizes, prev, pager, next"
    />
  </div>
</template>
