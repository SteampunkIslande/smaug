import { api, getApiMessage } from "../api.js";
import { setBusy, showMessage } from "../ui.js";

const form = document.getElementById("login-form");
const button = document.getElementById("login-button");

form?.addEventListener("submit", async (event) => {
  event.preventDefault();
  setBusy(button, true, "Connexion…");
  try {
    const credentials = Object.fromEntries(new FormData(form).entries());
    const response = await fetch(api.login, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(credentials),
    });

    if (response.ok) {
      window.location.assign(response.url || "/smaug/home");
      return;
    }

    let payload = null;
    try {
      payload = await response.json();
    } catch {
      payload = null;
    }
    showMessage("error", getApiMessage(payload) || "Identifiants invalides.");
    form.reset();
    form.querySelector("#usermail")?.focus();
  } catch (error) {
    showMessage("error", "Connexion impossible. Vérifiez votre réseau puis réessayez.");
    console.error(error);
  } finally {
    setBusy(button, false);
  }
});
