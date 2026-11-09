import init, {run} from "/gpui/harness_bridge_ui.js";

const loading = document.getElementById("loading");
const fail = error => {
  if (!loading.isConnected) document.body.append(loading);
  loading.querySelector("h1").textContent = "Could not open the interface";
  loading.querySelector("p").textContent = `${error?.message || error}\nKeep harness-bridge running and reload this page.`;
};
window.addEventListener("unhandledrejection", event => fail(event.reason));
try {
  await init();
  run();
} catch (error) { fail(error); }
