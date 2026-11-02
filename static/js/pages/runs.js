import { api, requestJson } from "../api.js";
import { createPagination } from "../pagination.js";
import { emptyState, renderTable, showError } from "../ui.js";

const PAGE_SIZE = 10;
const statusFilter = document.getElementById("status-filter");
const container = document.getElementById("runs-table");
const params = new URLSearchParams(window.location.search);
let status = params.get("status") || "";

if (statusFilter) statusFilter.value = status;

const pagination = createPagination({ onPage: (page) => load(page) });

async function load(page = 1) {
  emptyState(container, "Chargement des runs…");
  try {
    const data = await requestJson(api.listRuns({ page, pageSize: PAGE_SIZE, status }));
    renderTable(container, data, "Aucun run à afficher.");
    pagination.update(data?.pagination);
  } catch (error) {
    showError(error);
    emptyState(container, "Impossible de charger la liste des runs.");
  }
}

statusFilter?.addEventListener("change", () => {
  status = statusFilter.value;
  const url = new URL(window.location.href);
  if (status) url.searchParams.set("status", status);
  else url.searchParams.delete("status");
  window.history.replaceState({}, "", url);
  load(1);
});

await load(1);
