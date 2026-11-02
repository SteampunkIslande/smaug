import { api, requestJson } from "../api.js";
import { setBusy, showError, showMessage } from "../ui.js";

const form = document.getElementById("register-form");
const button = document.getElementById("create-user");

form?.addEventListener("submit", async (event) => {
  event.preventDefault();
  setBusy(button, true, "Création…");
  try {
    const payload = Object.fromEntries(new FormData(form).entries());
    await requestJson(api.register, { method: "POST", body: payload });
    showMessage("success", "Utilisateur créé avec succès.");
    form.reset();
    form.elements.usermail?.focus();
  } catch (error) {
    showError(error);
  } finally {
    setBusy(button, false);
  }
});
