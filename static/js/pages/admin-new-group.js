import { api, requestJson } from "../api.js";
import { setBusy, showError, showMessage } from "../ui.js";

const form = document.getElementById("new-group-form");
const button = document.getElementById("create-group");

form?.addEventListener("submit", async (event) => {
  event.preventDefault();
  const name = form.elements.new_group_name.value.trim();
  if (!name) {
    showMessage("error", "Veuillez entrer un nom de groupe.");
    return;
  }

  setBusy(button, true, "Création…");
  try {
    await requestJson(api.newGroup(name));
    showMessage("success", `Groupe « ${name} » créé avec succès.`);
    form.reset();
  } catch (error) {
    showError(error);
  } finally {
    setBusy(button, false);
  }
});
