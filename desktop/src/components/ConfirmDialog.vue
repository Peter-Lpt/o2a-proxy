<template>
  <div v-if="open" class="cd-mask" @mousedown.self="onCancel">
    <div class="cd-box" role="alertdialog" aria-modal="true" :aria-label="title">
      <div class="cd-title">{{ title }}</div>
      <div class="cd-msg">{{ message }}</div>
      <div class="cd-actions">
        <button type="button" class="btn" @click="onCancel">取消</button>
        <button type="button" class="btn btn-danger" @click="onOk">{{ okText }}</button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
defineProps<{
  open: boolean;
  title: string;
  message: string;
  okText?: string;
}>();
const emit = defineEmits<{ (e: "confirm"): void; (e: "cancel"): void }>();

function onOk() {
  emit("confirm");
}
function onCancel() {
  emit("cancel");
}
</script>

<style scoped>
.cd-mask {
  position: fixed;
  inset: 0;
  z-index: 200;
  background: rgba(2, 6, 16, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  animation: cd-in 0.12s ease-out;
}
@keyframes cd-in {
  from { opacity: 0; }
  to { opacity: 1; }
}
.cd-box {
  width: 320px;
  max-width: 86vw;
  background: var(--bg3);
  border: 1px solid var(--border);
  border-radius: 12px;
  box-shadow: var(--shadow);
  padding: 15px 16px 13px;
}
.cd-title {
  font-weight: 700;
  font-size: 13px;
  margin-bottom: 7px;
  color: var(--text);
}
.cd-msg {
  font-size: 12px;
  color: var(--muted);
  line-height: 1.6;
  margin-bottom: 14px;
  word-break: break-all;
  white-space: pre-line;
}
.cd-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
