import { api, requestJson } from "../api.js";
import { createPagination } from "../pagination.js";
import { emptyState, renderTable, setText, showError } from "../ui.js";

const form = document.getElementById("search-form");
const results = document.getElementById("search-results");
const count = document.getElementById("results-count");
let filters = null;

const pagination = createPagination({
  onPage: (page) => runSearch(page),
});

function readFilters() {
  const data = Object.fromEntries(new FormData(form).entries());
  return {
    name: data.run_name_search?.trim() || "",
    status: data.status || "",
    dateFrom: data.date_from || "",
    dateTo: data.date_to || "",
    pageSize: Math.min(100, Math.max(1, Number.parseInt(data.page_size, 10) || 10)),
  };
}

async function runSearch(page = 1) {
  if (!filters) filters = readFilters();
  emptyState(results, "Recherche en cours…");
  try {
    const data = await requestJson(api.searchRuns({ ...filters, page }));
    renderTable(results, data, "Aucun résultat trouvé.");
    pagination.update(data?.pagination);
    setText(count, `${data?.pagination?.total_count ?? 0} résultat(s)`);
  } catch (error) {
    showError(error);
    emptyState(results, "Impossible d’effectuer la recherche.");
    pagination.update(null);
    setText(count, "");
  }
}

form?.addEventListener("submit", (event) => {
  event.preventDefault();
  filters = readFilters();
  runSearch(1);
});
