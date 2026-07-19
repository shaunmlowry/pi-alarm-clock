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
    if (!res.ok) {
        let msg = `HTTP ${res.status}`;
        try {
            const err = await res.json();
            if (err && err.message) msg = err.message;
        } catch (_) {}
        throw new Error(msg);
    }
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
        <section id="calendarsSection">
            <h2>Google Calendars</h2>
            <div id="pairWrap">
                <button id="pairBtn">Pair Google Account</button>
                <div id="pairCode" hidden>
                    <p>On another device, visit <a id="pairUrl" target="_blank" rel="noopener"></a>
                    and enter code <strong id="pairUserCode"></strong>.</p>
                    <p class="muted" id="pairStatus">Waiting for you to authorize…</p>
                </div>
            </div>
            <div id="calendarsList"></div>
            <button id="discoverBtn">Discover Google Calendars</button>
            <div id="discoverWrap" hidden>
                <h3>Add a Calendar</h3>
                <select id="discoverSelect"></select>
                <div class="row">
                    <select id="calRole">
                        <option value="Agenda">Agenda</option>
                        <option value="Holiday">Holiday</option>
                    </select>
                    <button id="addDiscoveredBtn">Add</button>
                </div>
            </div>
            <h3>Add by ID</h3>
            <input id="calId" placeholder="Google Calendar ID (e.g. primary)">
            <input id="calName" placeholder="Display name">
            <div class="row">
                <select id="calRoleManual">
                    <option value="Agenda">Agenda</option>
                    <option value="Holiday">Holiday</option>
                </select>
                <button id="addCalBtn">Add</button>
            </div>
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

    let pairPoll = null;
    document.getElementById('pairBtn').onclick = async () => {
        const codeWrap = document.getElementById('pairCode');
        try {
            const data = await api('POST', '/calendars/pair', {});
            if (data.verification_url) {
                document.getElementById('pairUrl').textContent = data.verification_url;
                document.getElementById('pairUrl').href = data.verification_url;
                document.getElementById('pairUserCode').textContent = data.user_code;
                document.getElementById('pairStatus').textContent = 'Waiting for you to authorize…';
                codeWrap.hidden = false;
                // Poll for completion, then refresh the calendar list.
                if (pairPoll) clearInterval(pairPoll);
                pairPoll = setInterval(async () => {
                    try {
                        const st = await api('GET', '/calendars/pair/status');
                        document.getElementById('pairStatus').textContent =
                            st.state === 'paired' ? 'Paired! Refreshing calendars…'
                            : st.state === 'error' ? ('Error: ' + (st.message || 'failed'))
                            : 'Waiting for you to authorize…';
                        if (st.state === 'paired' || st.state === 'error') {
                            clearInterval(pairPoll);
                            pairPoll = null;
                            codeWrap.hidden = true;
                            loadCalendars();
                        }
                    } catch (_) { /* ignore transient poll errors */ }
                }, 3000);
            } else {
                // Surface the backend's error reason instead of a generic message.
                alert('Pairing failed: ' + (data.message || 'unexpected pairing response.'));
            }
        } catch (e) {
            alert('Pairing failed: ' + (e && e.message ? e.message : 'could not reach the device'));
        }
    };

    loadCalendars();

    document.getElementById('addCalBtn').onclick = async () => {
        const id = document.getElementById('calId').value.trim();
        const name = document.getElementById('calName').value.trim();
        const role = document.getElementById('calRoleManual').value;
        if (!id || !name) return alert('Fill calendar ID and name');
        await api('POST', '/calendars', { google_calendar_id: id, display_name: name, role });
        document.getElementById('calId').value = '';
        document.getElementById('calName').value = '';
        loadCalendars();
    };

    document.getElementById('discoverBtn').onclick = async () => {
        const btn = document.getElementById('discoverBtn');
        btn.disabled = true;
        try {
            const data = await api('POST', '/calendars/discover', {});
            const list = data.calendars || [];
            const sel = document.getElementById('discoverSelect');
            sel.innerHTML = list.map(c => `<option value="${c[0]}">${c[1]} (${c[0]})</option>`).join('');
            const wrap = document.getElementById('discoverWrap');
            wrap.hidden = list.length === 0;
            if (list.length === 0) alert('No Google calendars found, or the account is not paired on the Pi.');
        } catch (e) {
            alert('Discovery failed — is the Google account paired on the Pi?');
        } finally {
            btn.disabled = false;
        }
    };

    document.getElementById('addDiscoveredBtn').onclick = async () => {
        const sel = document.getElementById('discoverSelect');
        const id = sel.value;
        const name = sel.options[sel.selectedIndex].text.split(' (')[0];
        const role = document.getElementById('calRole').value;
        if (!id) return;
        await api('POST', '/calendars', { google_calendar_id: id, display_name: name, role });
        document.getElementById('discoverWrap').hidden = true;
        loadCalendars();
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

async function loadCalendars() {
    const data = await api('GET', '/calendars');
    const list = document.getElementById('calendarsList');
    if (!list) return;
    const calendars = data.calendars || [];
    list.innerHTML = calendars.length === 0
        ? '<p class="muted">No calendars configured.</p>'
        : calendars.map(c => `<div class="item"><strong>${escapeHtml(c.display_name)}</strong> <span class="muted">(${escapeHtml(c.role)})</span> <button data-id="${encodeURIComponent(c.google_calendar_id)}">Remove</button></div>`).join('');
    list.querySelectorAll('button[data-id]').forEach(btn => {
        btn.onclick = async () => {
            await api('DELETE', '/calendars/' + btn.dataset.id);
            loadCalendars();
        };
    });
}

function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, ch => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[ch]));
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
