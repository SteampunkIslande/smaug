import { api, requestJson } from "../api.js";
import { iconElement, setText, showError } from "../ui.js";

const body = document.getElementById("user-list");

function cell(children) {
  const td = document.createElement("td");
  td.append(...children);
  return td;
}

async function loadUsers() {
  try {
    const users = await requestJson(api.users);
    body.replaceChildren();
    setText("#users-count", `${users.length} compte${users.length > 1 ? "s" : ""}`);
    if (users.length === 0) {
      const row = document.createElement("tr");
      const td = document.createElement("td");
      td.colSpan = 3;
      td.className = "empty-state";
      td.textContent = "Aucun utilisateur.";
      row.append(td);
      body.append(row);
      return;
    }

    for (const user of users) {
      const row = document.createElement("tr");
      const name = document.createTextNode(user.username);
      const identity = [name];
      if (user.is_admin) {
        const badge = document.createElement("span");
        badge.className = "soft-badge";
        badge.append(iconElement("shield"), document.createTextNode(" Admin"));
        identity.push(" ", badge);
      }
      const groupsLink = document.createElement("a");
      groupsLink.href = `/smaug/admin/groupedit/${user.id}`;
      groupsLink.textContent = "Éditer les groupes";
      const passwordLink = document.createElement("a");
      passwordLink.href = `/smaug/passedit/${user.id}`;
      passwordLink.textContent = "Modifier le mot de passe";
      row.append(cell(identity), cell([groupsLink]), cell([passwordLink]));
      body.append(row);
    }
  } catch (error) {
    showError(error);
    body.replaceChildren();
  }
}

await loadUsers();
