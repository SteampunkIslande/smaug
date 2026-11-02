import { api, requestJson } from "../api.js";
import { emptyState, renderTable, showError } from "../ui.js";

async function loadRuns(status, prefix, emptyMessage) {
  const container = document.getElementById(`${prefix}-runs-table`);
  const overflow = document.getElementById(`${prefix}-runs-overflow`);
  if (!container) return;
  try {
    const data = await requestJson(api.listRuns({ page: 1, pageSize: 10, status }));
    renderTable(container, data, emptyMessage);
    if (overflow) overflow.hidden = !(data?.pagination?.total_pages > 1);
  } catch (error) {
    showError(error);
    emptyState(container, "Impossible de charger les runs.");
  }
}

await Promise.all([
  loadRuns("Running", "running", "Aucun run en cours."),
  loadRuns("Failure", "failure", "Aucun run en échec."),
]);
