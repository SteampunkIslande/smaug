import { api, requestJson } from "../api.js";
import { emptyState, setBusy, showError, showMessage } from "../ui.js";

const container = document.getElementById("groups-container");
const list = document.getElementById("group-list");
const saveButton = document.getElementById("save-groups");
const userId = Number(container?.dataset.userId);
let groups = [];
let initialIds = new Set();

function renderGroups() {
  list.replaceChildren();
  if (groups.length === 0) {
    emptyState(list, "Aucun groupe n’est défini.");
    return;
  }
  for (const group of groups) {
    const label = document.createElement("label");
    label.className = "check-row";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.name = "selected_groups";
    input.value = String(group.id);
    input.dataset.name = group.name;
    input.checked = initialIds.has(String(group.id));
    const text = document.createElement("span");
    text.textContent = group.name;
    label.append(input, text);
    list.append(label);
  }
}

async function loadGroups() {
  try {
    const [allGroups, userGroups] = await Promise.all([
      requestJson(api.groups),
      requestJson(api.userGroups(userId)),
    ]);
    groups = Array.isArray(allGroups) ? allGroups : [];
    initialIds = new Set((Array.isArray(userGroups) ? userGroups : []).map((group) => String(group.id)));
    renderGroups();
  } catch (error) {
    showError(error);
    emptyState(list, "Impossible de charger les groupes.");
  }
}

async function saveGroups() {
  const toAdd = [];
  const toRemove = [];
  for (const input of list.querySelectorAll("input[name='selected_groups']")) {
    const id = Number(input.value);
    const wasChecked = initialIds.has(input.value);
    if (input.checked && !wasChecked) toAdd.push({ id, name: input.dataset.name });
    if (!input.checked && wasChecked) toRemove.push({ id, name: input.dataset.name });
  }

  if (toAdd.length === 0 && toRemove.length === 0) {
    showMessage("info", "Aucune modification à enregistrer.");
    return;
  }

  setBusy(saveButton, true, "Enregistrement…");
  try {
    await requestJson(api.updateGroups, {
      method: "POST",
      body: { user_id: userId, to_add: toAdd, to_remove: toRemove },
    });
    showMessage("success", "Groupes mis à jour avec succès.");
    await loadGroups();
  } catch (error) {
    showError(error);
  } finally {
    setBusy(saveButton, false);
  }
}

saveButton?.addEventListener("click", saveGroups);
await loadGroups();
