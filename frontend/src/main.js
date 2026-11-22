import { createApp } from 'vue';
import { NButton, NCard, NCheckbox, NCheckboxGroup, NDataTable, NInput, NSelect, NTag } from 'naive-ui';
import App from './App.vue';
import './styles.css';

const app = createApp(App);

app.component('NButton', NButton);
app.component('NCard', NCard);
app.component('NCheckbox', NCheckbox);
app.component('NCheckboxGroup', NCheckboxGroup);
app.component('NDataTable', NDataTable);
app.component('NInput', NInput);
app.component('NSelect', NSelect);
app.component('NTag', NTag);

app.mount('#app');
