import { api, requestJson } from "../api.js";
import {
  emptyState,
  iconElement,
  readPageData,
  renderTable,
  setText,
  showError,
  showMessage,
} from "../ui.js";

const { groups = [], isAdmin = false } = readPageData();
const formList = document.getElementById("formList");
const formSearch = document.getElementById("formSearch");
const expansion = document.getElementById("domain-expansion");
const domainIdle = document.getElementById("domain-idle");
const domainContent = document.getElementById("domain-content");
const closeDomainButton = document.getElementById("closeDomain");
const allFormsButton = document.getElementById("showAllForms");
const expandedDomainTitle = document.getElementById("expanded-domain-title");
const resetFilters = document.getElementById("resetFilters");
const globalSearch = document.getElementById("globalSearch");
const noGroup = document.getElementById("no-user-group");
const count = document.getElementById("availableCount");
const domainCards = [...document.querySelectorAll("[data-category]")];

// Les listes restent produites par l'API de SMAUG (droits côté serveur).
const tabs = groups.map((group) => ({
  id: String(group.id), name: group.name, listKey: "with_group", forms: [],
}));
if (isAdmin) {
  tabs.push({ id: "None", name: "Sans groupe", listKey: "without_group", forms: [] });
}

const normalize = (value) => String(value ?? "")
  .normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();
const demultiplexTerms = ["demultiplex", "demux", "bcl"];
let activeDomain = null; // À l’ouverture, aucune liste ne masque le suivi des analyses.
let failedGroups = 0;

// Le clic ouvre un panneau accordéon sous les cartes, sans deuxième sélecteur.
// Ceci est seulement une présentation : l'API conserve le contrôle des droits.
function setupDomainDisclosure() {
  for (const card of domainCards) {
    card.addEventListener("click", () => {
      const next = activeDomain === card.dataset.category ? null : card.dataset.category;
      setActiveDomain(next);
    });
  }
  allFormsButton?.addEventListener("click", () => {
    setActiveDomain(activeDomain === "all" ? null : "all");
  });
  closeDomainButton?.addEventListener("click", () => setActiveDomain(null));
}

function setActiveDomain(domain) {
  const valid = domain === "all" || domainCards.some((card) => card.dataset.category === domain);
  const nextDomain = valid ? domain : null;
  if (nextDomain !== activeDomain) {
    // Ne pas conserver un ancien filtre texte lorsqu'on change de domaine.
    formSearch.value = "";
    if (globalSearch) globalSearch.value = "";
  }
  activeDomain = nextDomain;
  const isOpen = activeDomain !== null;
  domainIdle.hidden = isOpen;
  domainContent.hidden = !isOpen;
  for (const card of domainCards) {
    card.setAttribute("aria-expanded", String(card.dataset.category === activeDomain));
  }
  allFormsButton?.setAttribute("aria-expanded", String(activeDomain === "all"));
  if (!isOpen) {
    // Effacer la recherche cachée évite qu'elle affecte une prochaine ouverture.
    formSearch.value = "";
    if (globalSearch) globalSearch.value = "";
    return;
  }
  const card = domainCards.find((item) => item.dataset.category === activeDomain);
  const domainName = card?.querySelector("strong")?.textContent.trim();
  setText(expandedDomainTitle,
    activeDomain === "all" ? "Toutes les analyses disponibles" : `Analyses · ${domainName ?? "Domaine"}`);
  renderForms();
}

function formKey(form) {
  // Identité du formulaire dans un référentiel, pour éviter les doublons multi-groupes.
  if (form.form_id === "GitBranchAndPath") {
    return `${form.form_id}:${form.branch ?? ""}:${form.file_path ?? ""}`;
  }
  return JSON.stringify([form.form_id, form.name, form.branch, form.file_path]);
}

function accessibleForms() {
  const unique = new Map();
  for (const tab of tabs) {
    for (const form of tab.forms) {
      const key = formKey(form);
      if (!unique.has(key)) unique.set(key, { form, groupIds: new Set() });
      unique.get(key).groupIds.add(tab.id);
    }
  }
  return [...unique.values()];
}

function inDomain(entry) {
  if (activeDomain === "all") return true;
  const card = domainCards.find((element) => element.dataset.category === activeDomain);
  if (!card) return true;
  if (card.dataset.groupId) return entry.groupIds.has(card.dataset.groupId);
  // Démultiplexage : domaine commun à l'affichage, mais formulaires filtrés
  // sur les groupes réellement accessibles (aucun droit supplémentaire).
  if (activeDomain === "demultiplexage") {
    const groupLabelMatch = tabs.some((tab) => entry.groupIds.has(tab.id) &&
      demultiplexTerms.some((term) => normalize(tab.name).includes(term)));
    const formText = normalize(`${entry.form.name ?? ""} ${entry.form.description ?? ""}`);
    return groupLabelMatch || demultiplexTerms.some((term) => formText.includes(term));
  }
  return false;
}

function renderForms() {
  if (activeDomain === null) return;
  const query = normalize(formSearch.value.trim());
  const forms = accessibleForms().filter((entry) =>
    inDomain(entry) && (!query || normalize(`${entry.form.name ?? ""} ${entry.form.description ?? ""}`).includes(query))
  );
  formList.replaceChildren();
  resetFilters.hidden = !query;
  setText(count, `${forms.length} analyse${forms.length !== 1 ? "s" : ""}`);

  if (forms.length === 0) {
    let message = "Aucune analyse disponible dans ce domaine.";
    if (tabs.length === 0) message = "Aucune analyse accessible. Contactez votre administrateur.";
    else if (query) message = "Aucune analyse ne correspond à votre recherche.";
    else if (failedGroups) message = "Aucune analyse disponible ; certains groupes n'ont pas pu être chargés.";
    else if (activeDomain === "all") message = "Aucune analyse accessible pour vos groupes.";
    emptyState(formList, message);
    return;
  }

  for (const { form } of forms) {
    const item = document.createElement("article");
    item.className = "form-item domain-analysis-card";
    const copy = document.createElement("div");
    copy.className = "form-copy";
    const title = document.createElement("strong");
    title.textContent = form.name || "Analyse sans nom";
    const description = document.createElement("p");
    description.textContent = form.description || "";
    copy.append(title, description);

    const target = api.launchForm(form); // API et routes inchangées.
    if (target) {
      const link = document.createElement("a");
      link.className = "run-button";
      link.href = target;
      link.append(iconElement("play"), document.createTextNode("Ouvrir le formulaire"));
      item.append(copy, link);
    } else {
      const unavailable = document.createElement("span");
      unavailable.className = "soft-badge";
      unavailable.textContent = "Ouverture indisponible";
      item.append(copy, unavailable);
    }
    formList.append(item);
  }
}

async function loadForms() {
  if (tabs.length === 0) {
    if (noGroup) noGroup.hidden = false;
    renderForms();
    return;
  }
  const results = await Promise.allSettled(tabs.map(async (tab) => {
    const data = await requestJson(api.groupForms(tab.id));
    tab.forms = Array.isArray(data?.[tab.listKey]) ? data[tab.listKey] : [];
  }));
  failedGroups = results.filter((result) => result.status === "rejected").length;
  results.forEach((result, index) => {
    if (result.status === "rejected") {
      console.error(result.reason);
      showMessage("error", `Impossible de charger les analyses du groupe « ${tabs[index].name} ». `);
    }
  });
  renderForms();
}

async function loadRecentRuns() {
  const container = document.getElementById("latest-runs-table");
  try {
    const data = await requestJson(api.listRuns({ page: 1, pageSize: 5 }));
    const displayData = Array.isArray(data?.header) ? {
      ...data,
      header: data.header.map((column) => ({
        ...column,
        content: /^runs?$/i.test(String(column.content ?? "").trim()) ? "Analyse" : column.content,
      })),
    } : data;
    renderTable(container, displayData, "Aucune analyse récente.");
  } catch (error) {
    showError(error);
    emptyState(container, "Impossible de charger les analyses récentes.");
  }
}

setupDomainDisclosure();
formSearch?.addEventListener("input", renderForms);
globalSearch?.addEventListener("input", () => {
  // Mémoriser la saisie avant l'ouverture, qui réinitialise les anciens filtres.
  const query = globalSearch.value;
  if (activeDomain === null) setActiveDomain("all");
  formSearch.value = query;
  globalSearch.value = query;
  renderForms();
});
resetFilters?.addEventListener("click", () => {
  formSearch.value = "";
  if (globalSearch) globalSearch.value = "";
  renderForms();
});
await loadForms();
await loadRecentRuns();
