"use strict";

const { invoke } = window.__TAURI__.core;
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

function render(s) {
  snapshot = s;
  if (shown >= s.current.length) shown = 0;
  const w = s.current[shown];
  renderDisplays(s.current);

  if (w) {
    if ($("preview").getAttribute("src") !== w.preview_url) $("preview").src = w.preview_url;
    $("preview").alt = w.description || `Photo by ${w.author}`;
    $("photo").style.background = w.color || "";
    $("empty").hidden = true;
    $("location").hidden = !w.location;
    $("location-text").textContent = w.location || "";
    $("credit").textContent = `By ${w.author}`;
    $("credit").dataset.url = w.author_url;
    $("unsplash").dataset.url = w.photo_url;
  } else {
    $("preview").removeAttribute("src");
    $("photo").style.background = "";
    $("empty").hidden = false;
    $("empty").textContent = s.has_key ? "Fetching your first wallpaper…" : "Add an Unsplash key in Settings";
    $("location").hidden = true;
    $("credit").textContent = "";
    $("unsplash").dataset.url = "https://unsplash.com/";
  }

  $("btn-next").classList.toggle("spinning", s.busy);
  $("btn-next").disabled = s.busy || !s.has_key;
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

function openSettings() {
  const st = snapshot.settings;
  $("f-key").value = "";
  $("f-key").placeholder = snapshot.has_key ? "Leave blank to keep the current key" : "Paste your Access Key";
  $("f-query").value = st.query;
  $("f-topics").value = st.topics;
  const interval = String(st.interval_minutes);
  if (![...$("f-interval").options].some((o) => o.value === interval)) {
    $("f-interval").add(new Option(`${st.interval_minutes} minutes`, interval));
  }
  $("f-interval").value = interval;
  $("f-cap").value = st.cache_cap_mb;
  $("f-paused").checked = st.paused;
  $("f-login").checked = st.start_at_login;
  $("f-each").checked = st.per_display;
  $("view-main").hidden = true;
  $("view-settings").hidden = false;
  (snapshot.has_key ? $("f-query") : $("f-key")).focus();
}

function closeSettings() {
  $("view-settings").hidden = true;
  $("view-main").hidden = false;
}

async function saveSettings(event) {
  event.preventDefault();
  const settings = {
    query: $("f-query").value,
    topics: $("f-topics").value,
    interval_minutes: Number($("f-interval").value),
    paused: $("f-paused").checked,
    cache_cap_mb: Math.max(20, Number($("f-cap").value) || 200),
    start_at_login: $("f-login").checked,
    per_display: $("f-each").checked,
  };
  try {
    await invoke("save_settings", { settings, accessKey: $("f-key").value || null });
    closeSettings();
  } catch (e) {
    $("key-hint").textContent = String(e);
  }
}

function openLink(event) {
  event.preventDefault();
  const url = event.currentTarget.dataset.url;
  if (url) invoke("open_link", { url });
}

$("btn-next").addEventListener("click", () => invoke("next_wallpaper"));
$("btn-favorite").addEventListener("click", () => invoke("favorite", { index: shown }));
$("btn-folder").addEventListener("click", () => invoke("open_favorites"));
$("btn-settings").addEventListener("click", () => ($("view-settings").hidden ? openSettings() : closeSettings()));
$("btn-cancel").addEventListener("click", closeSettings);
$("btn-quit").addEventListener("click", () => invoke("quit"));
$("view-settings").addEventListener("submit", saveSettings);
$("credit").addEventListener("click", openLink);
$("unsplash").addEventListener("click", openLink);
$("dev-link").dataset.url = "https://unsplash.com/developers";
$("dev-link").addEventListener("click", openLink);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!$("view-settings").hidden) closeSettings();
    else invoke("hide_window");
  }
});

listen("state", (e) => render(e.payload));
listen("popover-shown", async () => {
  render(await invoke("get_state"));
  if (!snapshot.has_key && $("view-settings").hidden) openSettings();
});

invoke("get_state").then((s) => {
  render(s);
  if (!s.has_key) openSettings();
});
