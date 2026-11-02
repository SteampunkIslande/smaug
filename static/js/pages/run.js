import { api, requestJson } from "../api.js";
import { readPageData, redirectWithMessage, setText, showError, showMessage } from "../ui.js";

const { runId, attemptNumber } = readPageData("run-data");
const POLL_INTERVAL = 30000;

const stateClass = (state) => {
  const value = String(state || "").toUpperCase();
  if (value === "RUNNING" || value === "R") return "state-running";
  if (value === "PENDING" || value === "PD") return "state-pending";
  if (value === "COMPLETED" || value === "CD") return "state-completed";
  if (value === "FAILED" || value === "F") return "state-failed";
  if (["CANCELLED", "CA", "TIMEOUT", "TO"].includes(value)) return "state-cancelled";
  if (["SUSPENDED", "S", "CONFIGURING", "CF"].includes(value)) return "state-suspended";
  return "";
};

const isState = (state, values) => values.includes(String(state || "").toUpperCase());

async function act(action) {
  if (!runId) {
    showMessage("error", "Identifiant du run introuvable.");
    return;
  }

  if (action === "validate") {
    if (!window.confirm("Valider ce run ? Une nouvelle tentative sera créée et l’analyse pourra démarrer.")) return;
    try {
      const data = await requestJson(api.validateRun(runId), { method: "POST" });
      redirectWithMessage(`/smaug/show/run/${runId}`, "Succès", data?.message || "Run validé avec succès.");
    } catch (error) {
      showError(error);
    }
    return;
  }

  if (action === "retry") {
    if (!window.confirm("Relancer ce run avec la version actuelle du pipeline ? Les variables restent identiques.")) return;
    try {
      const data = await requestJson(api.retryRun(runId));
      redirectWithMessage(`/smaug/show/run/${runId}`, "Succès", data?.message || "Le run est à nouveau à valider.");
    } catch (error) {
      showError(error);
    }
  }
}

for (const button of document.querySelectorAll("[data-run-action]")) {
  button.addEventListener("click", () => act(button.dataset.runAction));
}

const progressBar = document.getElementById("progress-bar");
const progressContainer = document.getElementById("progress-container");
const refreshButton = document.getElementById("refresh-btn");
const watchStatus = document.getElementById("watch-status");
let intervalId = null;
let pollInFlight = false;
let currentJobName = "";

function setWatchStatus(message) {
  if (!watchStatus) return;
  watchStatus.textContent = message || "";
  watchStatus.hidden = !message;
}

function setProgress(percent) {
  if (!progressBar) return;
  const value = Math.max(0, Math.min(100, Number(percent) || 0));
  progressBar.style.width = `${value}%`;
  progressBar.textContent = `${value}%`;
  progressContainer?.setAttribute("aria-valuenow", String(value));
}

function setStep(step) {
  const element = document.getElementById("current-step");
  if (!element) return;
  element.textContent = step || "Non renseignée";
}

function setLastLine(line) {
  const element = document.getElementById("last-output-line");
  if (!element) return;
  element.textContent = line || "Aucune ligne de sortie pour le moment.";
  element.classList.toggle("is-empty", !line);
}

function renderJobs(jobs) {
  const table = document.getElementById("slurm-jobs-table");
  const body = document.getElementById("slurm-jobs-body");
  const message = document.getElementById("slurm-jobs-message");
  if (!table || !body || !message) return;

  body.replaceChildren();
  if (!Array.isArray(jobs) || jobs.length === 0) {
    table.hidden = true;
    message.hidden = false;
    message.textContent = currentJobName
      ? "Aucun job SLURM en attente ou en cours pour ce nom."
      : "Aucun nom de job SLURM détecté dans la sortie du pipeline.";
    return;
  }

  for (const job of jobs) {
    const row = document.createElement("tr");
    const values = [job.job_id, job.job_name, job.state, job.elapsed, job.time_limit, job.nodes, job.nodelist, job.reason];
    values.forEach((value, index) => {
      const cell = document.createElement("td");
      if (index === 2) {
        const badge = document.createElement("span");
        badge.className = `state-badge ${stateClass(value)}`;
        badge.textContent = value || "—";
        cell.append(badge);
      } else {
        cell.textContent = value || "—";
      }
      row.append(cell);
    });
    body.append(row);
  }

  message.hidden = true;
  table.hidden = false;
}

async function refreshSqueue(jobName) {
  currentJobName = String(jobName || "").trim();
  if (!currentJobName) {
    setText("#pending-jobs-count", "—");
    setText("#running-jobs-count", "—");
    renderJobs([]);
    return;
  }

  try {
    const jobs = await requestJson(api.squeue(runId, attemptNumber));
    const safeJobs = Array.isArray(jobs) ? jobs : [];
    setText("#pending-jobs-count", String(safeJobs.filter((job) => isState(job.state, ["PENDING", "PD"])).length));
    setText("#running-jobs-count", String(safeJobs.filter((job) => isState(job.state, ["RUNNING", "R"])).length));
    renderJobs(safeJobs);
  } catch (error) {
    setText("#pending-jobs-count", "—");
    setText("#running-jobs-count", "—");
    renderJobs([]);
    const message = document.getElementById("slurm-jobs-message");
    if (message) message.textContent = `Impossible de récupérer les jobs SLURM : ${error.message}`;
    console.warn(error);
  }
}

async function pollJobInfo() {
  if (pollInFlight || !runId) return;
  pollInFlight = true;
  if (refreshButton) refreshButton.disabled = true;
  try {
    const info = await requestJson(api.watch(runId, attemptNumber));
    const data = info || {};
    setStep(data.current_step_string);
    setLastLine(data.last_output_line);
    setProgress(data.current_progress);
    setText("#done-jobs-count", data.done_jobs_count ?? "—");
    setText("#slurm-job-name", data.slurm_job_name ? `Job SLURM : ${data.slurm_job_name}` : "Aucun job SLURM associé");
    setWatchStatus(data.log_available === false ? "La progression n’est pas encore disponible." : "");
    await refreshSqueue(data.slurm_job_name);
  } catch (error) {
    setWatchStatus("Erreur de communication avec le serveur. Nouvelle tentative automatique.");
    console.warn(error);
  } finally {
    if (refreshButton) refreshButton.disabled = false;
    pollInFlight = false;
  }
}

function stopPolling() {
  if (intervalId) window.clearInterval(intervalId);
  intervalId = null;
}

if (progressBar) {
  refreshButton?.addEventListener("click", pollJobInfo);
  window.addEventListener("pagehide", stopPolling);
  pollJobInfo();
  intervalId = window.setInterval(pollJobInfo, POLL_INTERVAL);
}

const advancedTabs = document.querySelectorAll("[data-advanced-tab]");
const scriptOutput = document.getElementById("run-script");
let scriptLoaded = false;

function selectAdvancedTab(name) {
  for (const button of advancedTabs) {
    const selected = button.dataset.advancedTab === name;
    button.setAttribute("aria-selected", String(selected));
    const panel = document.getElementById(`advanced-panel-${button.dataset.advancedTab}`);
    if (panel) panel.hidden = !selected;
  }
}

async function loadScript() {
  if (!scriptOutput || scriptLoaded) return;
  scriptLoaded = true;
  const url = scriptOutput.dataset.scriptUrl;
  try {
    const response = await fetch(url, { headers: { Accept: "text/plain" } });
    if (!response.ok) throw new Error(`Erreur HTTP ${response.status}`);
    scriptOutput.textContent = await response.text();
    scriptOutput.classList.remove("is-empty");
  } catch (error) {
    scriptLoaded = false;
    scriptOutput.textContent = `Impossible de charger le script : ${error.message}`;
    scriptOutput.classList.add("is-empty");
  }
}

for (const button of advancedTabs) {
  button.addEventListener("click", () => {
    selectAdvancedTab(button.dataset.advancedTab);
    if (button.dataset.advancedTab === "script") loadScript();
  });
}
