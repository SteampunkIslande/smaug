const API_ROOT = "/smaug/api";

export class ApiError extends Error {
  constructor(message, status = 0) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export function isApiSuccess(payload) {
  if (!payload || typeof payload !== "object") return false;
  if (Object.hasOwn(payload, "success")) return Boolean(payload.success);
  return Object.hasOwn(payload, "Success");
}

export function getApiData(payload) {
  if (!payload || typeof payload !== "object") return undefined;
  if (Object.hasOwn(payload, "data")) return payload.data;
  if (payload.Success && typeof payload.Success === "object") return payload.Success.data;
  return undefined;
}

export function getApiMessage(payload) {
  if (!payload || typeof payload !== "object") return undefined;
  if (Object.hasOwn(payload, "message")) return payload.message;
  if (payload.Failure && typeof payload.Failure === "object") return payload.Failure.message;
  return undefined;
}

async function parsePayload(response) {
  const text = await response.text();
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return { Failure: { message: text.slice(0, 300) } };
  }
}

export async function requestJson(url, options = {}) {
  const { body, headers, ...rest } = options;
  const response = await fetch(url, {
    ...rest,
    headers: {
      Accept: "application/json",
      ...(body instanceof FormData ? {} : { "Content-Type": "application/json" }),
      ...headers,
    },
    body: body instanceof FormData || typeof body === "string" ? body : body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await parsePayload(response);
  if (!response.ok || !isApiSuccess(payload)) {
    throw new ApiError(getApiMessage(payload) || `Erreur HTTP ${response.status}`, response.status);
  }
  return getApiData(payload);
}

const encode = encodeURIComponent;
const encodePath = (path = "") => String(path).split("/").map(encode).join("/");

export const api = Object.freeze({
  login: `${API_ROOT}/login`,
  register: `${API_ROOT}/register`,
  password: `${API_ROOT}/passedit`,
  newGroup: (name) => `${API_ROOT}/newgroup/${encode(name)}`,
  groups: `${API_ROOT}/groups/list`,
  userGroups: (userId) => `${API_ROOT}/groups/list/${encode(userId)}`,
  updateGroups: `${API_ROOT}/groups/update`,
  groupForms: (groupId) => `${API_ROOT}/groups/forms/${encode(groupId)}`,
  users: `${API_ROOT}/users/list`,
  updateFormsCache: `${API_ROOT}/update/cache/forms`,
  newRun: `${API_ROOT}/newrun`,
  editRun: `${API_ROOT}/editrun`,
  validateRun: (runId) => `${API_ROOT}/validate/${encode(runId)}`,
  retryRun: (runId) => `${API_ROOT}/retry/${encode(runId)}`,
  upload: `${API_ROOT}/upload`,
  watch: (runId, attemptNumber) => `${API_ROOT}/watch/${encode(runId)}/${encode(attemptNumber)}`,
  squeue: (runId, attemptNumber) => `${API_ROOT}/squeue?${new URLSearchParams({ run_id: String(runId), attempt_number: String(attemptNumber) })}`,
  configSet: `${API_ROOT}/config/set`,
  listRuns: ({ page = 1, pageSize = 5, status = "" } = {}) => {
    const params = new URLSearchParams({ page: String(page), page_size: String(pageSize) });
    if (status) params.set("status", status);
    return `${API_ROOT}/listruns?${params}`;
  },
  searchRuns: ({ page = 1, pageSize = 5, status, dateFrom, dateTo, name } = {}) => {
    const params = new URLSearchParams({ page: String(page), page_size: String(pageSize) });
    if (status) params.set("status", status);
    if (dateFrom) params.set("date_from", dateFrom);
    if (dateTo) params.set("date_to", dateTo);
    if (name) params.set("run_name_search", name);
    return `${API_ROOT}/searchrun?${params}`;
  },
  redirect: ({ targetUrl, title, message, seconds = 5 }) => {
    const params = new URLSearchParams({
      target_url: targetUrl,
      title,
      message,
      seconds: String(seconds),
    });
    return `${API_ROOT}/redirect?${params}`;
  },
  launchForm: (form) => {
    if (!form || form.form_id !== "GitBranchAndPath" || !form.branch || !form.file_path) return null;
    return `/smaug/runs/submit/branch/${encode(form.branch)}/${encodePath(form.file_path)}`;
  },
});

export async function fetchFormChoices(source) {
  const response = await fetch(source, { headers: { Accept: "application/json" } });
  if (!response.ok) throw new ApiError(`HTTP ${response.status} en interrogeant ${source}`, response.status);
  const payload = await response.json();
  const values = isApiSuccess(payload) ? getApiData(payload) : payload;
  if (!Array.isArray(values)) throw new ApiError(`Réponse inattendue de ${source}`);
  return values.map((item) => {
    if (item && typeof item === "object") {
      const value = item.path ?? item.value ?? item.id ?? item.name;
      const label = item.label ?? item.name ?? item.title ?? item.path ?? String(value);
      return { value: String(value), label: String(label) };
    }
    return { value: String(item), label: String(item) };
  });
}
