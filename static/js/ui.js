import { ApiError, api, getApiData, getApiMessage, isApiSuccess } from "./api.js";

const MESSAGE_IDS = {
  error: "error-message",
  info: "info-message",
  success: "success-message",
  warning: "warning-message",
};

export function iconElement(name, className = "icon") {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", className);
  svg.setAttribute("aria-hidden", "true");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.append(use);
  return svg;
}

export function showMessage(type, message) {
  const normalized = MESSAGE_IDS[type] ? type : "info";
  for (const id of Object.values(MESSAGE_IDS)) {
    const element = document.getElementById(id);
    if (element) element.hidden = true;
  }
  const element = document.getElementById(MESSAGE_IDS[normalized]);
  if (!element) return;
  element.className = `alert alert--${normalized}`;
  element.textContent = message instanceof Error ? message.message : String(message ?? "");
  element.hidden = false;
  window.scrollTo({ top: 0, behavior: "smooth" });
  if (normalized !== "error") {
    window.clearTimeout(element._hideTimer);
    element._hideTimer = window.setTimeout(() => { element.hidden = true; }, 4200);
  }
}

export function showError(error) {
  showMessage("error", error instanceof ApiError ? error.message : error?.message || String(error));
}

export function readPageData(id = "page-data") {
  const element = document.getElementById(id);
  if (!element) return {};
  try {
    return JSON.parse(element.textContent || "{}");
  } catch {
    return {};
  }
}

export function setText(selector, value, root = document) {
  const element = typeof selector === "string" ? root.querySelector(selector) : selector;
  if (element) element.textContent = value ?? "";
}

export function setBusy(element, busy, busyLabel = "Chargement…") {
  if (!element) return;
  if (busy) {
    element.dataset.originalLabel = element.textContent;
    element.textContent = busyLabel;
    element.disabled = true;
  } else {
    element.textContent = element.dataset.originalLabel || element.textContent;
    element.disabled = false;
    delete element.dataset.originalLabel;
  }
}

export function renderTable(container, payload, emptyMessage = "Aucune donnée à afficher.") {
  if (!container) return;
  const data = isApiSuccess(payload) ? getApiData(payload) : payload;
  if (!data || !Array.isArray(data.header) || !Array.isArray(data.table)) {
    throw new Error(getApiMessage(payload) || "Format de réponse inattendu");
  }

  container.replaceChildren();
  if (data.table.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-state";
    empty.textContent = emptyMessage;
    container.append(empty);
    return;
  }

  const table = document.createElement("table");
  table.className = "data-table";
  const thead = document.createElement("thead");
  const headerRow = document.createElement("tr");
  for (const column of data.header) {
    const th = document.createElement("th");
    if (column.class) th.className = column.class;
    th.textContent = column.content ?? "";
    headerRow.append(th);
  }
  thead.append(headerRow);

  const tbody = document.createElement("tbody");
  for (const row of data.table) {
    const tr = document.createElement("tr");
    for (const cell of row) {
      const td = document.createElement("td");
      if (cell.td_class) td.className = cell.td_class;
      if (cell.href) {
        const link = document.createElement("a");
        link.href = cell.href;
        link.textContent = cell.content ?? "";
        td.append(link);
      } else if (cell.class) {
        const span = document.createElement("span");
        span.className = cell.class;
        span.textContent = cell.content ?? "";
        td.append(span);
      } else {
        td.textContent = cell.content ?? "";
      }
      tr.append(td);
    }
    tbody.append(tr);
  }

  table.append(thead, tbody);
  container.append(table);
}

export function goTo(url) {
  window.location.assign(url);
}

export function redirectWithMessage(targetUrl, title, message, seconds = 5) {
  goTo(api.redirect({ targetUrl, title, message, seconds }));
}

export function emptyState(container, text) {
  if (!container) return;
  const p = document.createElement("p");
  p.className = "empty-state";
  p.textContent = text;
  container.replaceChildren(p);
}
