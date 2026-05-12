/**
 * etag Node Manager — frontend application.
 *
 * Communicates with the Rust backend via Tauri commands (`invoke`).
 * The backend in turn calls the etag-bridge gRPC server.
 *
 * Panels:
 *   dashboard  — live node table with stats cards (auto-refreshes every 3 s)
 *   provision  — BLE Mesh provisioning form (WIP)
 *   link       — link a node to a Grocy item
 *   settings   — configure the bridge gRPC address
 */

import { invoke } from "@tauri-apps/api/core";

// ── State ─────────────────────────────────────────────────────────────────────

let refreshTimer = null;

// ── Boot ──────────────────────────────────────────────────────────────────────

document.addEventListener("DOMContentLoaded", async () => {
  setupNav();
  setupProvisionForm();
  setupLinkForm();
  await setupSettingsForm();
  switchTab("dashboard");
});

// ── Navigation ────────────────────────────────────────────────────────────────

function setupNav() {
  document.querySelectorAll("[data-tab]").forEach((el) => {
    el.addEventListener("click", () => switchTab(el.dataset.tab));
  });
}

function switchTab(tab) {
  // Update nav active class.
  document.querySelectorAll("[data-tab]").forEach((el) => {
    el.classList.toggle("active", el.dataset.tab === tab);
  });

  // Show / hide panels.
  document.querySelectorAll(".tab-panel").forEach((panel) => {
    panel.classList.add("hidden");
  });
  document.getElementById(`panel-${tab}`)?.classList.remove("hidden");

  // Dashboard auto-refresh.
  if (tab === "dashboard") {
    loadNodes();
    if (!refreshTimer) {
      refreshTimer = setInterval(loadNodes, 3000);
    }
  } else {
    clearInterval(refreshTimer);
    refreshTimer = null;
  }
}

// ── Dashboard ─────────────────────────────────────────────────────────────────

document.getElementById("btn-refresh")?.addEventListener("click", loadNodes);

async function loadNodes() {
  try {
    const nodes = await invoke("list_nodes");
    renderTable(nodes);
    renderStats(nodes);
  } catch (err) {
    showGlobalAlert(`Bridge unreachable: ${err}`, "error");
    renderTableError(err);
  }
}

function renderTable(nodes) {
  const tbody = document.getElementById("node-table-body");
  if (!tbody) return;

  if (nodes.length === 0) {
    tbody.innerHTML = `
      <tr>
        <td colspan="8" class="text-center py-10 text-base-content/40">
          No nodes discovered. Ensure <code class="bg-base-300 px-1 rounded text-xs">etag-bridge</code> is running.
        </td>
      </tr>`;
    return;
  }

  tbody.innerHTML = nodes.map((n) => {
    const status = statusBadge(n);
    const product = n.product_name
      ? `<span class="text-sm">${esc(n.product_name)}</span>
         <br><span class="font-mono text-xs text-base-content/50">${esc(n.grocycode ?? "")}</span>`
      : `<span class="text-base-content/30 text-xs italic">Not linked</span>`;
    const stock = n.stock_count != null
      ? `<span class="font-mono font-semibold">${n.stock_count}</span>`
      : `<span class="text-base-content/30">—</span>`;
    const battery = n.battery_pct != null ? batteryBadge(n.battery_pct) : `<span class="text-base-content/30">—</span>`;
    const rssi = n.rssi != null
      ? `<span class="font-mono text-xs">${n.rssi}&nbsp;dBm</span>`
      : `<span class="text-base-content/30">—</span>`;
    const lastSeen = relativeTime(n.last_seen_unix);

    return `
      <tr class="hover cursor-default">
        <td class="font-mono text-xs whitespace-nowrap">${esc(n.address)}</td>
        <td class="text-sm">${esc(n.name ?? n.address)}</td>
        <td>${status}</td>
        <td>${product}</td>
        <td class="text-center">${stock}</td>
        <td class="text-center">${battery}</td>
        <td class="text-center">${rssi}</td>
        <td class="text-xs text-base-content/60 whitespace-nowrap">${lastSeen}</td>
      </tr>`;
  }).join("");
}

function renderTableError(err) {
  const tbody = document.getElementById("node-table-body");
  if (!tbody) return;
  tbody.innerHTML = `
    <tr>
      <td colspan="8" class="text-center py-10">
        <span class="text-error text-sm">Could not load nodes: ${esc(String(err))}</span>
      </td>
    </tr>`;
}

function renderStats(nodes) {
  const connected = nodes.filter((n) => n.connected).length;
  const lowBat    = nodes.filter((n) => n.battery_pct != null && n.battery_pct <= 10).length;
  const unlinked  = nodes.filter((n) => !n.grocycode).length;
  setText("stat-total",     nodes.length);
  setText("stat-connected", connected);
  setText("stat-low-bat",   lowBat);
  setText("stat-unlinked",  unlinked);
}

// ── Provision form ────────────────────────────────────────────────────────────

function setupProvisionForm() {
  document.getElementById("provision-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const form    = /** @type {HTMLFormElement} */ (e.target);
    const address = form.elements.namedItem("address").value.trim();
    const key     = form.elements.namedItem("mesh_network_key").value.trim() || null;
    const resultEl = document.getElementById("provision-result");

    setResult(resultEl, "info", "Sending…");
    try {
      const r = await invoke("provision_node", { address, meshNetworkKey: key });
      setResult(resultEl, r.success ? "success" : "warning", r.message);
    } catch (err) {
      setResult(resultEl, "error", String(err));
    }
  });
}

// ── Link form ─────────────────────────────────────────────────────────────────

function setupLinkForm() {
  document.getElementById("link-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const form      = /** @type {HTMLFormElement} */ (e.target);
    const address   = form.elements.namedItem("address").value.trim();
    const grocycode = form.elements.namedItem("grocycode").value.trim();
    const resultEl  = document.getElementById("link-result");

    setResult(resultEl, "info", "Sending…");
    try {
      const r = await invoke("link_node_to_grocy", { address, grocycode });
      setResult(resultEl, r.success ? "success" : "error", r.message);
      if (r.success) form.reset();
    } catch (err) {
      setResult(resultEl, "error", String(err));
    }
  });
}

// ── Settings form ─────────────────────────────────────────────────────────────

async function setupSettingsForm() {
  // Pre-populate the input with the current bridge address.
  try {
    const addr = await invoke("get_bridge_addr");
    const input = document.getElementById("bridge-addr");
    if (input) input.value = addr;
  } catch (_) { /* ignore */ }

  document.getElementById("config-form")?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const form  = /** @type {HTMLFormElement} */ (e.target);
    const addr  = form.elements.namedItem("bridge-addr").value.trim();
    const resEl = document.getElementById("config-result");

    try {
      await invoke("set_bridge_addr", { addr });
      setResult(resEl, "success", "Bridge address updated.");
    } catch (err) {
      setResult(resEl, "error", String(err));
    }
  });
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function statusBadge(node) {
  if (node.connected) return `<span class="badge badge-success badge-sm">Connected</span>`;
  switch (node.status) {
    case "active":       return `<span class="badge badge-info badge-sm">Active</span>`;
    case "away":         return `<span class="badge badge-ghost badge-sm">Away</span>`;
    case "provisioning": return `<span class="badge badge-warning badge-sm">Provisioning</span>`;
    default:             return `<span class="badge badge-ghost badge-sm">Unknown</span>`;
  }
}

function batteryBadge(pct) {
  const color = pct <= 10 ? "text-error" : pct <= 20 ? "text-warning" : "text-success";
  return `<span class="font-mono text-xs ${color}">${pct}%</span>`;
}

/**
 * Convert a Unix timestamp (seconds) to a human-readable relative string.
 * @param {number} unix
 */
function relativeTime(unix) {
  const diff = Math.floor(Date.now() / 1000) - unix;
  if (diff < 0)    return "just now";
  if (diff < 60)   return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  return `${Math.floor(diff / 3600)}h ago`;
}

/** Escape HTML special characters. */
function esc(str) {
  return String(str)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Set an element's text content. */
function setText(id, value) {
  const el = document.getElementById(id);
  if (el) el.textContent = String(value);
}

/**
 * Show an alert result inside a container element.
 * @param {HTMLElement|null} el
 * @param {"info"|"success"|"warning"|"error"} type
 * @param {string} message
 */
function setResult(el, type, message) {
  if (!el) return;
  el.className = `alert alert-${type} mt-3 text-sm`;
  el.textContent = message;
  el.classList.remove("hidden");
}

/**
 * Show a transient global alert in the navbar area.
 * @param {string} message
 * @param {"info"|"success"|"warning"|"error"} type
 */
function showGlobalAlert(message, type = "info") {
  const el = document.getElementById("global-alert");
  if (!el) return;
  el.className = `alert alert-${type} text-xs py-1 px-3 max-w-xs`;
  el.textContent = message;
  el.classList.remove("hidden");
  setTimeout(() => el.classList.add("hidden"), 4000);
}
