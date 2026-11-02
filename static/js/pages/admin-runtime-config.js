import { api, requestJson } from "../api.js";
import { setBusy, showError, showMessage } from "../ui.js";

document.getElementById("config-body")?.addEventListener("click", async (event) => {
  const button = event.target.closest("[data-config-key]");
  if (!button) return;

  const key = button.dataset.configKey;
  const row = document.querySelector(`[data-config-row="${CSS.escape(key)}"]`);
  const input = row?.querySelector(`[name="${CSS.escape(key)}"]`);
  if (!input) return;

  setBusy(button, true, "…");
  try {
    await requestJson(api.configSet, { method: "POST", body: { key, value: input.value } });
    showMessage("success", `Configuration « ${key} » sauvegardée.`);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(button, false);
  }
});
