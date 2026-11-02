import { api, requestJson } from "../api.js";
import { setBusy, showError, showMessage } from "../ui.js";

const button = document.getElementById("update-forms-cache");

button?.addEventListener("click", async () => {
  setBusy(button, true, "Mise à jour…");
  try {
    await requestJson(api.updateFormsCache);
    showMessage("success", "Cache des formulaires mis à jour.");
  } catch (error) {
    showError(error);
  } finally {
    setBusy(button, false);
  }
});
