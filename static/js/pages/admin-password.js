import { api, requestJson } from "../api.js";
import { readPageData, setBusy, showError, showMessage } from "../ui.js";

const { userId, isAdmin } = readPageData();
const form = document.getElementById("password-form");
const button = document.getElementById("save-password");

form?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const oldPassword = form.elements.old_password?.value || null;
  const newPassword = form.elements.new_password.value;
  const confirmation = form.elements.new_password_confirm.value;

  if (!isAdmin && !oldPassword) {
    showMessage("error", "Veuillez entrer votre mot de passe actuel.");
    return;
  }
  if (!newPassword) {
    showMessage("error", "Veuillez entrer un nouveau mot de passe.");
    return;
  }
  if (newPassword !== confirmation) {
    showMessage("error", "Les deux mots de passe ne correspondent pas.");
    return;
  }

  setBusy(button, true, "Enregistrement…");
  try {
    await requestJson(api.password, {
      method: "POST",
      body: { old_password: oldPassword, new_password: newPassword, user_id: Number(userId) },
    });
    showMessage("success", "Mot de passe mis à jour avec succès.");
    window.setTimeout(() => {
      window.location.assign(isAdmin ? "/smaug/admin/editusers" : "/smaug/home");
    }, 900);
  } catch (error) {
    showError(error);
  } finally {
    setBusy(button, false);
  }
});
