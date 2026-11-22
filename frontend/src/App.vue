<script setup>
import { NConfigProvider, NMessageProvider, dateZhCN, zhCN } from 'naive-ui';
import AppShell from './AppShell.vue';

const themeOverrides = {
  common: {
    primaryColor: '#3978f6',
    primaryColorHover: '#2e65d0',
    primaryColorPressed: '#275bbb',
    borderRadius: '10px',
  },
};
</script>

<template>
  <NConfigProvider :locale="zhCN" :date-locale="dateZhCN" :theme-overrides="themeOverrides">
    <NMessageProvider>
      <AppShell />
    </NMessageProvider>
  </NConfigProvider>
</template>
