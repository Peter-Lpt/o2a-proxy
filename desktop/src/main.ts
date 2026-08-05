import { createApp } from "vue";
import "./styles.css";
import PanelApp from "./PanelApp.vue";
import FloatApp from "./FloatApp.vue";
import { applyTheme } from "./theme";

applyTheme();
const isFloat = window.location.hash.startsWith("#/float");
createApp(isFloat ? FloatApp : PanelApp).mount("#app");
