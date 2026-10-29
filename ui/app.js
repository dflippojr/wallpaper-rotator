"use strict";

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let snapshot = null;
// Which display's photo the popover shows when each has its own.
let shown = 0;

function timeUntil(epochSeconds) {
  const minutes = Math.round((epochSeconds * 1000 - Date.now()) / 60000);
  if (minutes <= 1) return "soon";
  if (minutes < 60) return `in ${minutes} min`;
  const hours = Math.round(minutes / 60);
  if (hours < 36) return `in ${hours} h`;
  return `in ${Math.round(hours / 24)} days`;
}

function renderDisplays(photos) {
  const bar = $("displays");
  bar.hidden = photos.length < 2;
  bar.replaceChildren(
    ...photos.map((photo, i) => {
      const button = document.createElement("button");
      button.textContent = photo.display || `Display ${i + 1}`;
      button.title = `Show the photo on ${button.textContent}`;
      button.setAttribute("aria-pressed", String(i === shown));
      button.addEventListener("click", () => {
        shown = i;
        render(snapshot);
      });
      return button;
    }),
  );
}

// Hosted previews where the source has one; the cached copy otherwise.
function previewSrc(w) {
  if (w.preview_url) return w.preview_url;
  return w.preview_file ? convertFileSrc(w.preview_file) : "";
}

function render(s) {
  snapshot = s;
  if (shown >= s.current.length) shown = 0;
  const w = s.current[shown];
  renderDisplays(s.current);

  if (w) {
    const src = previewSrc(w);
    if ($("preview").getAttribute("src") !== src) $("preview").src = src;
    $("preview").alt = w.description || `Photo by ${w.author}`;
    $("photo").style.background = w.color || "";
    $("empty").hidden = true;
    // The location, or else the caption (Bing and Commons have no location).
    const place = w.location || w.description;
    $("location").hidden = !place;
    $("location-text").textContent = place || "";
    $("location").classList.toggle("caption", !w.location);
    $("credit").textContent = `By ${w.author}`;
    $("credit").dataset.url = w.author_url;
    $("source-link").textContent = w.source;
    $("source-link").dataset.url = w.photo_url;
    $("source-link").classList.toggle("plain", !w.photo_url);
  } else {
    $("preview").removeAttribute("src");
    $("photo").style.background = "";
    $("empty").hidden = false;
    $("empty").textContent = s.needs_setup ? "Pick a photo source in Settings" : "Fetching your first wallpaper…";
    $("location").hidden = true;
    $("credit").textContent = "";
    $("source-link").textContent = "";
  }

  const nextTitle = s.current.length > 1 && w ? `New photo for ${w.display || `display ${shown + 1}`}` : "Next wallpaper";
  $("btn-next").title = nextTitle;
  $("btn-next").setAttribute("aria-label", nextTitle);
  $("btn-next").classList.toggle("spinning", s.busy);
  $("btn-next").disabled = s.busy || s.needs_setup;
  // Until there is a photo, the empty-state text says what's happening.
  $("btn-next").hidden = !w;
  $("btn-favorite").disabled = !w;

  const status = $("status");
  status.classList.toggle("error", Boolean(s.error));
  if (s.error) status.textContent = s.error;
  else if (s.busy) status.textContent = "Getting a new wallpaper…";
  else if (s.notice) status.textContent = s.notice;
  else if (s.settings.paused) status.textContent = "Rotation paused";
  else if (s.next_rotation_at && w) status.textContent = `Next wallpaper ${timeUntil(s.next_rotation_at)}`;
  else status.textContent = "";
}

const sourceBoxes = () => [...document.querySelectorAll("input[data-source]")];
const ticked = (name) => sourceBoxes().some((b) => b.dataset.source === name && b.checked);

// Key fields and Topics only show for the sources that use them.
function syncSourceFields() {
  document.querySelector('[data-settings-panel="more"]').hidden = !ticked("unsplash");
  if (!ticked("unsplash") && !$("settings-more").hidden) showSettingsPanel("sources");
  $("unsplash-fields").hidden = !ticked("unsplash");
  $("pexels-fields").hidden = !ticked("pexels");
  $("topics-field").hidden = !ticked("unsplash");
  $("search-field").hidden = !["unsplash", "pexels", "wallhaven"].some(ticked);
}

function showSettingsPanel(name) {
  for (const button of document.querySelectorAll("[data-settings-panel]")) {
    const active = button.dataset.settingsPanel === name;
    button.setAttribute("aria-pressed", String(active));
    $("settings-" + button.dataset.settingsPanel).hidden = !active;
  }
}

function openSettings() {
  const st = snapshot.settings;
  for (const box of sourceBoxes()) box.checked = st.sources.includes(box.dataset.source);
  $("f-key").value = "";
  $("f-key").placeholder = snapshot.has_unsplash_key ? "Leave blank to keep the current key" : "Paste your Access Key";
  $("f-pexels").value = "";
  $("f-pexels").placeholder = snapshot.has_pexels_key ? "Leave blank to keep the current key" : "Paste your API key";
  $("f-query").value = st.query;
  $("f-topics").value = st.topics;
  $("f-collections").value = st.collections || "";
  $("f-username").value = st.username || "";
  $("username-ignored").textContent = snapshot.photographer_note || "";
  showSettingsPanel("sources");
  const interval = String(st.interval_minutes);
  if (![...$("f-interval").options].some((o) => o.value === interval)) {
    $("f-interval").add(new Option(`${st.interval_minutes} minutes`, interval));
  }
  $("f-interval").value = interval;
  $("f-cap").value = st.cache_cap_mb;
  $("f-paused").checked = st.paused;
  $("f-login").checked = st.start_at_login;
  $("f-each").checked = st.per_display;
  $("settings-error").textContent = "";
  syncSourceFields();
  $("view-main").hidden = true;
  $("view-settings").hidden = false;
  $("view-settings").scrollTop = 0;
  sourceBoxes()[0].focus();
}

function closeSettings() {
  $("view-settings").hidden = true;
  $("view-main").hidden = false;
}

async function saveSettings(event) {
  event.preventDefault();
  const settings = {
    sources: sourceBoxes().filter((b) => b.checked).map((b) => b.dataset.source),
    query: $("f-query").value,
    topics: $("f-topics").value,
    collections: $("f-collections").value,
    username: $("f-username").value,
    interval_minutes: Number($("f-interval").value),
    paused: $("f-paused").checked,
    // The floor is MIN_CACHE_MB in crates/core/src/config.rs (also `min` in index.html).
    cache_cap_mb: Math.max(20, Number($("f-cap").value) || 200),
    start_at_login: $("f-login").checked,
    per_display: $("f-each").checked,
  };
  if (settings.sources.length === 0) {
    $("settings-error").textContent = "Tick at least one photo source.";
    return;
  }
  try {
    await invoke("save_settings", {
      settings,
      accessKey: $("f-key").value || null,
      pexelsKey: $("f-pexels").value || null,
    });
    closeSettings();
  } catch (e) {
    $("settings-error").textContent = String(e);
  }
}

function openLink(event) {
  event.preventDefault();
  const url = event.currentTarget.dataset.url;
  if (url) invoke("open_link", { url });
}

// With a photo per display, only the display being shown changes.
$("btn-next").addEventListener("click", () =>
  invoke("next_wallpaper", { index: snapshot && snapshot.current.length > 1 ? shown : null }),
);
$("btn-favorite").addEventListener("click", () => invoke("favorite", { index: shown }));
$("btn-folder").addEventListener("click", () => invoke("open_favorites"));
$("btn-settings").addEventListener("click", () => ($("view-settings").hidden ? openSettings() : closeSettings()));
$("btn-cancel").addEventListener("click", closeSettings);
$("btn-quit").addEventListener("click", () => invoke("quit"));
$("view-settings").addEventListener("submit", saveSettings);
for (const button of document.querySelectorAll("[data-settings-panel]")) {
  button.addEventListener("click", () => showSettingsPanel(button.dataset.settingsPanel));
}
for (const box of sourceBoxes()) box.addEventListener("change", syncSourceFields);
// The note describes the saved settings; editing them makes it stale.
for (const id of ["f-query", "f-collections", "f-username"]) {
  $(id).addEventListener("input", () => ($("username-ignored").textContent = ""));
}
$("credit").addEventListener("click", openLink);
$("source-link").addEventListener("click", openLink);
$("dev-link").dataset.url = "https://unsplash.com/developers";
$("dev-link").addEventListener("click", openLink);
$("pexels-link").dataset.url = "https://www.pexels.com/api/";
$("pexels-link").addEventListener("click", openLink);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!$("view-settings").hidden) closeSettings();
    else invoke("hide_window");
  }
});

listen("state", (e) => render(e.payload));
listen("popover-shown", async () => {
  render(await invoke("get_state"));
  if (snapshot.needs_setup && $("view-settings").hidden) openSettings();
});

invoke("get_state").then((s) => {
  render(s);
  if (s.needs_setup) openSettings();
});
