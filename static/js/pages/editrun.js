import { api, fetchFormChoices, requestJson } from "../api.js";
import { readPageData, redirectWithMessage, showError, showMessage } from "../ui.js";

const { form, user, run } = readPageData();
const container = document.getElementById("form-variables");
const formElement = document.getElementById("run-form");
const saveButton = document.getElementById("save-run");
const currentValues = run?.user_defined_vars || {};

const makeOption = (value, label, selected = false) => {
  const option = document.createElement("option");
  option.value = String(value);
  option.textContent = String(label);
  option.selected = selected;
  return option;
};

const fieldShell = (variable) => {
  const wrapper = document.createElement("div");
  wrapper.className = "field";
  wrapper.dataset.varName = variable.name;
  wrapper.dataset.varType = variable.type || "LineEdit";
  const label = document.createElement("label");
  label.htmlFor = `var-${variable.name}`;
  label.textContent = variable.title || variable.name;
  wrapper.append(label);
  return wrapper;
};

function appendHelp(wrapper, variable) {
  if (!variable.description) return;
  const help = document.createElement("small");
  help.className = "field-help";
  help.textContent = variable.description;
  wrapper.append(help);
}

function renderText(variable, current, type = "text") {
  const wrapper = fieldShell(variable);
  const input = document.createElement("input");
  input.type = type;
  input.id = `var-${variable.name}`;
  input.name = variable.name;
  input.value = current;
  wrapper.append(input);
  appendHelp(wrapper, variable);
  return wrapper;
}

function renderSelect(variable, current, choices) {
  const wrapper = fieldShell(variable);
  const select = document.createElement("select");
  select.id = `var-${variable.name}`;
  select.name = variable.name;
  select.append(makeOption("", "— Sélectionner une valeur —", !current));
  const known = choices.some((choice) => choice.value === current);
  if (current && !known) select.append(makeOption(current, `${current} (valeur actuelle)`, true));
  for (const choice of choices) {
    select.append(makeOption(choice.value, choice.label, choice.value === current));
  }
  wrapper.append(select);
  appendHelp(wrapper, variable);
  return wrapper;
}

async function renderVariable(variable) {
  const current = String(currentValues[variable.name] ?? "");
  const type = variable.type || "LineEdit";

  if (type === "DateEdit") return renderText(variable, current, "date");
  if (type === "LineEdit") return renderText(variable, current);

  if (type === "ExistingFile") {
    const wrapper = fieldShell(variable);
    const hidden = document.createElement("input");
    hidden.type = "hidden";
    hidden.id = `var-${variable.name}`;
    hidden.name = variable.name;
    hidden.value = current;

    const file = document.createElement("input");
    file.type = "file";
    file.dataset.role = "file";

    const status = document.createElement("small");
    status.className = "var-file-status";
    if (current) status.textContent = `Fichier actuel : ${current}`;

    file.addEventListener("change", async () => {
      if (!file.files?.length) return;
      const data = new FormData();
      data.append("file", file.files[0]);
      data.append("file_name_base", file.files[0].name);
      status.textContent = "Envoi du fichier…";
      try {
        const path = await requestJson(api.upload, { method: "POST", body: data });
        hidden.value = path;
        status.textContent = `Fichier envoyé : ${path}`;
      } catch (error) {
        hidden.value = "";
        status.textContent = "";
        showError(error);
      }
    });

    wrapper.append(file, hidden, status);
    appendHelp(wrapper, variable);
    return wrapper;
  }

  if (type === "ValuesList") {
    const choices = (variable.values || []).map((value) => ({ value: String(value), label: String(value) }));
    return renderSelect(variable, current, choices);
  }

  if (type === "ListFromURL") {
    const wrapper = fieldShell(variable);
    const select = document.createElement("select");
    select.id = `var-${variable.name}`;
    select.name = variable.name;
    select.append(makeOption("", "Chargement…"));
    select.disabled = true;
    const status = document.createElement("small");
    status.className = "var-source-status";
    wrapper.append(select, status);
    appendHelp(wrapper, variable);
    try {
      const choices = await fetchFormChoices(variable.source);
      select.replaceChildren();
      select.append(makeOption("", "— Sélectionner une valeur —", !current));
      const known = choices.some((choice) => choice.value === current);
      if (current && !known) select.append(makeOption(current, `${current} (valeur actuelle)`, true));
      for (const choice of choices) {
        select.append(makeOption(choice.value, choice.label, choice.value === current));
      }
      select.disabled = false;
    } catch (error) {
      status.textContent = "Source indisponible, saisie libre activée.";
      const fallback = document.createElement("input");
      fallback.type = "text";
      fallback.id = `var-${variable.name}`;
      fallback.name = variable.name;
      fallback.value = current;
      select.replaceWith(fallback);
      showError(error);
    }
    return wrapper;
  }

  return renderText(variable, current);
}

function collectValues() {
  const values = {};
  for (const wrapper of container.querySelectorAll("[data-var-name]")) {
    const name = wrapper.dataset.varName;
    const type = wrapper.dataset.varType;
    let value = "";
    if (type === "ExistingFile") {
      value = wrapper.querySelector("input[type='hidden']")?.value || "";
    } else if (type === "ValuesList" || type === "ListFromURL") {
      value = wrapper.querySelector("select")?.value ?? wrapper.querySelector("input[type='text']")?.value ?? "";
    } else if (type === "DateEdit") {
      value = wrapper.querySelector("input[type='date']")?.value || "";
    } else {
      value = wrapper.querySelector("input[type='text']")?.value || "";
    }
    values[name] = value;
  }
  return values;
}

async function submitRun() {
  if (!formElement.reportValidity()) return;
  saveButton.disabled = true;
  try {
    const userDefinedVars = collectValues();
    let payload;
    let target;

    if (run) {
      payload = { ...run, user_defined_vars: userDefinedVars };
      const data = await requestJson(api.editRun, { method: "POST", body: payload });
      target = `/smaug/show/run/${run.run_id}`;
      redirectWithMessage(target, "Succès", data?.message || "Run modifié avec succès.");
      return;
    }

    const runName = document.getElementById("run-name")?.value.trim() || "";
    if (!runName) {
      showMessage("error", "Le nom du run est obligatoire.");
      return;
    }
    if (!form.file_path || !form.branch) {
      showMessage("error", "Formulaire incomplet : branche ou fichier manquant.");
      return;
    }

    payload = {
      user_id: user.id,
      run_name: runName,
      user_defined_vars: userDefinedVars,
      form_id: "GitBranchAndPath",
      file_path: form.file_path,
      branch: form.branch,
      creation_date: null,
      status: "Idle",
      attempt_count: 0,
    };
    const data = await requestJson(api.newRun, { method: "POST", body: payload });
    target = `/smaug/show/run/${data.run_id}`;
    redirectWithMessage(target, "Succès", data.message || "Run créé avec succès.");
  } catch (error) {
    showError(error);
  } finally {
    saveButton.disabled = false;
  }
}

if (container && formElement) {
  const variables = Array.isArray(form?.variables) ? form.variables : [];
  if (variables.length === 0) {
    container.replaceChildren();
    const message = document.createElement("p");
    message.className = "empty-state";
    message.textContent = "Aucune variable définie pour ce formulaire.";
    container.append(message);
  } else {
    const fields = await Promise.all(variables.map(renderVariable));
    container.replaceChildren(...fields);
  }
  formElement.addEventListener("submit", (event) => {
    event.preventDefault();
    submitRun();
  });
}
