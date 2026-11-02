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
const groupTabs = document.getElementById("groupTabs");
const formList = document.getElementById("formList");
const formSearch = document.getElementById("formSearch");
const resetFilters = document.getElementById("resetFilters");
const globalSearch = document.getElementById("globalSearch");
const categoryFeedback = document.getElementById("categoryFeedback");
const noGroup = document.getElementById("no-user-group");
const count = document.getElementById("availableCount");

const tabs = groups.map((group) => ({
  id: String(group.id),
  name: group.name,
  listKey: "with_group",
  forms: [],
}));

if (isAdmin) {
  tabs.push({ id: "None", name: "Sans groupe", listKey: "without_group", forms: [] });
}

let activeTab = tabs[0]?.id ?? null;

const hints = {
  demultiplexage: ["demultiplex", "demux", "bcl"],
  genetique: ["geneti", "constitution", "germinal", "wes", "wgs"],
  onco: ["onco", "hemato", "somati", "cancer"],
  immunologie: ["immun"],
  autres: ["outil", "autre", "qualite", "qc", "annotation"],
};

const normalize = (value) =>
  String(value ?? "")
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase();

function renderTabs() {
  groupTabs.replaceChildren();
  if (tabs.length === 0) return;
  for (const tab of tabs) {
    const button = document.createElement("button");
    button.type = "button";
    button.role = "tab";
    button.dataset.group = tab.id;
    button.textContent = tab.name;
    button.setAttribute("aria-selected", String(tab.id === activeTab));
    button.addEventListener("click", () => {
      activeTab = tab.id;
      renderTabs();
      renderForms();
    });
    groupTabs.append(button);
  }
}

function renderForms() {
  const tab = tabs.find((item) => item.id === activeTab);
  if (!tab) {
    emptyState(formList, "Aucun formulaire disponible.");
    setText(count, "0 formulaire");
    return;
  }

  const query = normalize(formSearch.value.trim());
  const forms = tab.forms.filter((form) =>
    !query || normalize(`${form.name} ${form.description}`).includes(query)
  );

  formList.replaceChildren();
  resetFilters.hidden = !query;
  setText(count, `${forms.length} formulaire${forms.length > 1 ? "s" : ""}`);

  if (forms.length === 0) {
    emptyState(formList, query ? "Aucun formulaire ne correspond à votre recherche." : "Aucun formulaire dans ce groupe.");
    return;
  }

  for (const form of forms) {
    const item = document.createElement("article");
    item.className = "form-item";

    const copy = document.createElement("div");
    copy.className = "form-copy";
    const title = document.createElement("strong");
    title.textContent = form.name || "Formulaire sans nom";
    const description = document.createElement("p");
    description.textContent = form.description || "";
    copy.append(title, description);

    const target = api.launchForm(form);
    if (target) {
      const link = document.createElement("a");
      link.className = "run-button";
      link.href = target;
      link.append(iconElement("play"), document.createTextNode("Lancer un run"));
      item.append(copy, link);
    } else {
      const unavailable = document.createElement("span");
      unavailable.className = "soft-badge";
      unavailable.title = "Ce formulaire n’a pas de route de lancement dans cette version de SMAUG.";
      unavailable.textContent = "Lancement indisponible";
      item.append(copy, unavailable);
    }
    formList.append(item);
  }
}

function findCategoryGroup(terms) {
  return tabs.find((tab) =>
    tab.forms.some((form) => terms.some((term) => normalize(`${form.name} ${form.description}`).includes(term)))
  );
}

function chooseCategory(category) {
  const terms = hints[category] || [];
  const tab = findCategoryGroup(terms);
  if (categoryFeedback) {
    categoryFeedback.hidden = Boolean(tab);
    categoryFeedback.textContent = tab
      ? ""
      : "Aucun formulaire de cette catégorie n’est disponible dans vos groupes. Consultez la liste ci-dessous.";
  }
  if (tab) {
    activeTab = tab.id;
    const matchingTerm = terms.find((term) =>
      tab.forms.some((form) => normalize(`${form.name} ${form.description}`).includes(term))
    );
    formSearch.value = matchingTerm || "";
    renderTabs();
    renderForms();
  }
  document.getElementById("forms")?.scrollIntoView({ behavior: "smooth", block: "start" });
}

async function loadForms() {
  if (tabs.length === 0) {
    noGroup.hidden = false;
    renderForms();
    return;
  }

  const results = await Promise.allSettled(
    tabs.map(async (tab) => {
      const data = await requestJson(api.groupForms(tab.id));
      tab.forms = Array.isArray(data?.[tab.listKey]) ? data[tab.listKey] : [];
    })
  );

  results.forEach((result, index) => {
    if (result.status === "rejected") {
      console.error(result.reason);
      showMessage("error", `Impossible de charger les formulaires du groupe « ${tabs[index].name} ».`);
    }
  });

  renderTabs();
  renderForms();
}

async function loadRecentRuns() {
  const container = document.getElementById("latest-runs-table");
  try {
    const data = await requestJson(api.listRuns({ page: 1, pageSize: 5 }));
    renderTable(container, data, "Aucun run récent.");
  } catch (error) {
    showError(error);
    emptyState(container, "Impossible de charger les runs récents.");
  }
}

formSearch?.addEventListener("input", renderForms);
globalSearch?.addEventListener("input", () => {
  formSearch.value = globalSearch.value;
  renderForms();
});
resetFilters?.addEventListener("click", () => {
  formSearch.value = "";
  if (globalSearch) globalSearch.value = "";
  if (categoryFeedback) categoryFeedback.hidden = true;
  renderForms();
});
document.querySelectorAll("[data-category]").forEach((card) => {
  card.addEventListener("click", () => chooseCategory(card.dataset.category));
});
document.querySelectorAll("[data-scroll-to]").forEach((button) => {
  button.addEventListener("click", () => {
    document.getElementById(button.dataset.scrollTo)?.scrollIntoView({ behavior: "smooth", block: "start" });
  });
});

await loadForms();
await loadRecentRuns();
