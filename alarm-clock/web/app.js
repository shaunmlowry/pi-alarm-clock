// Minimal vanilla JS config client for the Pi Alarm Clock (slice 8 v1).
// Reads the bearer token from the URL hash on first pair, then stores it in
// localStorage for repeat visits. Communicates with the Pi over HTTPS via the
// axum REST API.

const TOKEN_KEY = 'alarm-clock-token';

function getToken() {
    const hash = new URLSearchParams(window.location.hash.slice(1));
    const token = hash.get('token');
    if (token) {
        localStorage.setItem(TOKEN_KEY, token);
        // Remove token from URL so it is not accidentally shared.
        window.location.hash = '';
    }
    return localStorage.getItem(TOKEN_KEY);
}

function apiUrl(path) {
    // The page is served from the Pi, so the same host + port works.
    return `${window.location.origin}/api${path}`;
}

async function api(method, path, body) {
    const token = localStorage.getItem(TOKEN_KEY);
    const opts = {
        method,
        headers: { 'Authorization': `Bearer ${token}`, 'Content-Type': 'application/json' },
    };
    if (body !== undefined) opts.body = JSON.stringify(body);
    const res = await fetch(apiUrl(path), opts);
    if (res.status === 401) {
        localStorage.removeItem(TOKEN_KEY);
        renderUnpaired('Session expired — please scan the QR again.');
        throw new Error('unauthorized');
    }
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.json();
}

function renderUnpaired(message) {
    const app = document.getElementById('app');
    app.innerHTML = `
        <section>
            <h2>Not Paired</h2>
            <p>${message || 'Scan the QR code on the Pi touchscreen to pair this device.'}</p>
        </section>
    `;
    document.getElementById('revokeBtn').hidden = true;
}

function renderConfig() {
    const app = document.getElementById('app');
    document.getElementById('revokeBtn').hidden = false;
    app.innerHTML = `
        <section id="alarmsSection">
            <h2>Alarms</h2>
            <div id="alarmsList"></div>
            <h3>New Alarm</h3>
            <input id="alarmName" placeholder="Name">
            <input id="alarmTime" type="time" value="07:00">
            <input id="alarmSource" placeholder="Source URI (radio stream, podcast, etc.)">
            <button id="addAlarmBtn">Add Alarm</button>
        </section>
        <section id="favoritesSection">
            <h2>Favorites</h2>
            <div id="favoritesList"></div>
            <h3>New Favorite</h3>
            <input id="favName" placeholder="Name">
            <input id="favUri" placeholder="Source URI">
            <select id="favType">
                <option value="stream">Stream</option>
                <option value="podcast">Podcast Feed</option>
            </select>
            <button id="addFavBtn">Add Favorite</button>
        </section>
        <section>
            <h2>Weather City</h2>
            <div class="row">
                <input id="weatherCity" placeholder="City name">
                <button id="saveCityBtn">Save</button>
            </div>
        </section>
        <section>
            <h2>Theme</h2>
            <select id="themeSelect">
                <option value="Liquid Glass">Liquid Glass</option>
                <option value="Dark">Dark</option>
                <option value="Light">Light</option>
            </select>
            <button id="saveThemeBtn">Save Theme</button>
        </section>
        <section>
            <h2>Display Brightness Floor</h2>
            <input id="brightnessFloor" type="range" min="0" max="100" value="10">
            <button id="saveBrightnessBtn">Save</button>
        </section>
    `;

    document.getElementById('addAlarmBtn').onclick = async () => {
        const name = document.getElementById('alarmName').value;
        const time = document.getElementById('alarmTime').value;
        const source = document.getElementById('alarmSource').value;
        if (!name || !time || !source) return alert('Fill all fields');
        await api('POST', '/alarms', {
            id: crypto.randomUUID(),
            enabled: true,
            name,
            time_local: time,
            timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
            rrule: null,
            source_uri: source,
            max_volume: 100,
            snooze_minutes: 5,
            max_snoozes: 3,
            holiday_policy: 'none',
        });
        loadAlarms();
    };

    document.getElementById('addFavBtn').onclick = async () => {
        const name = document.getElementById('favName').value;
        const uri = document.getElementById('favUri').value;
        const type = document.getElementById('favType').value;
        if (!name || !uri) return alert('Fill all fields');
        await api('POST', '/favorites', {
            id: crypto.randomUUID(),
            name,
            source_uri: uri,
            source_type: type,
        });
        loadFavorites();
    };

    document.getElementById('saveCityBtn').onclick = async () => {
        await api('PUT', '/weather-city', { city: document.getElementById('weatherCity').value });
    };

    document.getElementById('saveThemeBtn').onclick = async () => {
        await api('PUT', '/theme', { theme: document.getElementById('themeSelect').value });
    };

    document.getElementById('saveBrightnessBtn').onclick = async () => {
        await api('PUT', '/display', { brightness_floor: parseInt(document.getElementById('brightnessFloor').value, 10) });
    };

    loadAlarms();
    loadFavorites();
}

async function loadAlarms() {
    const data = await api('GET', '/alarms');
    const list = document.getElementById('alarmsList');
    if (!list) return;
    const alarms = data.alarms || [];
    list.innerHTML = alarms.length === 0
        ? '<p class="muted">No alarms yet.</p>'
        : alarms.map(a => `<div class="item"><strong>${a.name}</strong> — ${a.time_local}<br><span class="muted">${a.source_uri}</span></div>`).join('');
}

async function loadFavorites() {
    const data = await api('GET', '/favorites');
    const list = document.getElementById('favoritesList');
    if (!list) return;
    const favorites = data.favorites || [];
    list.innerHTML = favorites.length === 0
        ? '<p class="muted">No favorites yet.</p>'
        : favorites.map(f => `<div class="item"><strong>${f.name}</strong> <span class="muted">(${f.source_type})</span></div>`).join('');
}

document.getElementById('revokeBtn').onclick = async () => {
    await api('DELETE', '/revoke');
    localStorage.removeItem(TOKEN_KEY);
    renderUnpaired('Pairing revoked. Scan the QR again to re-pair.');
};

function init() {
    if (!getToken()) {
        renderUnpaired();
        return;
    }
    renderConfig();
}

init();
