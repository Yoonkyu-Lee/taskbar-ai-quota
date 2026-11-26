const $ = (s) => document.querySelector(s);
const send = (op, args = {}) =>
  window.ipc.postMessage(JSON.stringify({ op, ...args }));
const escapeText = (s) =>
  String(s ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[
        c
      ],
  );
const names = { Claude: "Claude", Codex: "GPT" };
const logos = { Claude: "__CLAUDE__", Codex: "__CODEX__" };
let accounts = [],
  active = null,
  tab = "limits",
  day = 29,
  defaults = {},
  folders = {},
  pendingAdd = false;
const date = (t) =>
  new Date(t * 1000).toLocaleString("ko-KR", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
const cost = (v) =>
  v == null
    ? "비용 미상"
    : new Intl.NumberFormat("en-US", {
        style: "currency",
        currency: "USD",
      }).format(v);
const tokens = (v) =>
  new Intl.NumberFormat("en-US", {
    notation: "compact",
    maximumFractionDigits: 2,
  }).format(v);
function reading(d, exact = false) {
  return d.tokens == null
    ? "기록 없음"
    : cost(d.cost) +
        " · " +
        (exact ? d.tokens.toLocaleString() : tokens(d.tokens)) +
        " 토큰";
}
function resetTime(t) {
  if (t == null) return "초기화 시각 미제공";
  const m = Math.ceil((t * 1000 - Date.now()) / 60000);
  if (m <= 0) return date(t) + " · 다음 조회 대기";
  return (
    [
      Math.floor(m / 1440) + "일",
      Math.floor((m % 1440) / 60) + "시간",
      (m % 60) + "분",
    ]
      .filter((x) => !x.startsWith("0"))
      .join(" ") +
    " 후 초기화 · " +
    date(t)
  );
}
function limit(label, key, v) {
  if (!Number.isFinite(v?.remaining)) return "";
  const used = 100 - Math.max(0, Math.min(100, v.remaining));
  return (
    '<div class="limit"><div class="line"><span>' +
    label +
    "</span><b>" +
    Number(used.toFixed(1)) +
    '% 사용</b></div><div class="meter"><i style="width:' +
    used +
    "%;background:var(--" +
    key +
    ')"></i></div><small>' +
    resetTime(v.resets_at) +
    "</small></div>"
  );
}
function usage(a) {
  const d = a.days,
    peak = Math.max(...d.map((x) => x.tokens ?? 0)),
    known = d.filter((x) => x.tokens != null);
  if (!known.length)
    return (
      '<p class="empty">최근 30일간 이 계정에 귀속된 기록이 없습니다.</p>' +
      historyNote(a)
    );
  const total = {
    tokens: known.reduce((n, d) => n + d.tokens, 0),
    cost: known.every((d) => d.cost != null)
      ? known.reduce((n, d) => n + d.cost, 0)
      : null,
  };
  const bars = d
    .map(
      (v, i) =>
        '<button data-day="' +
        i +
        '" aria-pressed="' +
        (i === day) +
        '" aria-label="' +
        v.date +
        " " +
        reading(v, true) +
        '"><i style="height:' +
        (peak ? (100 * v.tokens) / peak : 0) +
        '%"></i></button>',
    )
    .join("");
  return (
    '<div class="line"><h3>일별 토큰 사용량</h3><small>최근 30일</small></div><div class="chart">' +
    bars +
    '</div><div class="line"><small>' +
    d[0].date.slice(5) +
    "</small><small>" +
    d[29].date.slice(5) +
    "</small></div><output>" +
    d[day].date +
    " · " +
    reading(d[day], true) +
    '</output><dl class="summary">' +
    [
      ["오늘", d[29]],
      ["어제", d[28]],
      ["최근 30일", total],
    ]
      .map(
        ([k, v]) => "<div><dt>" + k + "</dt><dd>" + reading(v) + "</dd></div>",
      )
      .join("") +
    "</dl>" +
    historyNote(a)
  );
}
function historyNote(a) {
  return (
    '<p class="muted">' +
    (a.provider === "Claude" ? "Claude Code" : "Codex") +
    " 로컬 로그 · API 환산 추정 비용</p>" +
    (!a.dedicated
      ? '<p class="muted">계정 ID가 확인된 기록만 포함합니다.</p>'
      : "") +
    '<p class="muted">' +
    escapeText(a.scan_note) +
    "</p>"
  );
}
function detail(a) {
  const s = a.snapshot || {},
    credits = s.credits;
  const quotas =
    limit("주간 한도", "week", s.week) +
    limit("Fable 주간 한도", "fable", s.fable) +
    limit("5시간 한도", "short", s.short);
  $("#detail").innerHTML =
    '<div class="tabs"><button data-tab="limits">할당량</button><button data-tab="history">사용 이력</button></div><div class="pages"><section data-page="limits">' +
    (quotas || '<p class="empty">확인된 한도가 없습니다.</p>') +
    (a.provider === "Codex"
      ? '<div class="credits line"><span>리셋권</span><b>' +
        (credits ? credits.count + "장" : "정보 없음") +
        "</b></div>" +
        (credits?.expires_at
          ? "<small>만료 " + date(credits.expires_at) + "</small>"
          : "")
      : "") +
    '</section><section data-page="history">' +
    usage(a) +
    "</section></div>";
  tabs();
}
function tabs() {
  document
    .querySelectorAll("[data-page]")
    .forEach((p) => (p.inert = p.dataset.page !== tab));
  document
    .querySelectorAll("[data-tab]")
    .forEach((b) => b.setAttribute("aria-pressed", b.dataset.tab === tab));
}
function render() {
  const a = accounts.find((x) => x.id === active);
  if (!a) active = null;
  $("#detail").hidden = !a;
  $("#manage").hidden = !!a;
  $("#settings").hidden = !a;
  $("#refresh").hidden = !a;
  $("#brand").className = a ? "logo " + a.provider : "";
  $("#brand").innerHTML = a ? logos[a.provider] : "";
  $("#heading").textContent = a ? names[a.provider] + " · " + a.alias : "Quota";
  $("#sub").textContent = a
    ? [a.plan, a.email].filter(Boolean).join(" · ") || "계정 상세"
    : "작업표시줄 계정 설정";
  $("#stamp").textContent = a
    ? a.success
      ? "최근 성공 " + date(a.success)
      : "첫 조회 대기"
    : "Windows · Quota __VERSION__";
  if (a) {
    $("#refresh").textContent = a.locked ? "원래 계정 다시 확인" : "새로 고침";
    detail(a);
    $("#msg").textContent = a.status === "정상" ? "" : a.status;
    return;
  }
  $("#accounts").innerHTML =
    accounts
      .map(
        (x, i) =>
          '<div class="account"><button class="title" data-act="open" data-id="' +
          x.id +
          '">' +
          names[x.provider] +
          " · " +
          escapeText(x.alias) +
          '</button><p class="muted">' +
          escapeText([x.email, x.plan].filter(Boolean).join(" · ")) +
          '</p><p class="muted">' +
          escapeText(x.folder) +
          '</p><p class="muted">' +
          escapeText(x.status) +
          '</p><div class="actions"><label><input type="checkbox" data-act="show" data-id="' +
          x.id +
          '" ' +
          (x.visible ? "checked" : "") +
          ">표시</label>" +
          [
            ["up", "↑", i === 0],
            ["down", "↓", i === accounts.length - 1],
            ["rename", "별칭", false],
            ["remove", "연결 해제", false],
          ]
            .map(
              ([k, label, off]) =>
                "<button " +
                (off ? "disabled" : "") +
                ' data-id="' +
                x.id +
                '" data-act="' +
                k +
                '">' +
                label +
                "</button>",
            )
            .join("") +
          "</div></div>",
      )
      .join("") ||
    '<p class="empty">Windows에서 로그인한 프로필을 연결하세요.</p>';
  updateFolder();
}
window.quota = {
  state(items) {
    accounts = items;
    render();
  },
  defaults(paths) {
    defaults = paths;
    updateFolder();
  },
  folder(provider, path, fresh = false) {
    folders[provider] = path;
    $("#dedicated").checked = fresh;
    $("#form-status").textContent = fresh ? "로그인을 마쳤습니다. 계정을 확인하는 중입니다…" : "";
    updateFolder();
  },
  open(id) {
    active = id;
    tab = "limits";
    day = 29;
    $("#msg").textContent = "";
    render();
    window.scrollTo(0, 0);
  },
  registered(id) {
    pendingAdd = false;
    active = id;
    tab = "limits";
    day = 29;
    $("#form-status").textContent = "";
    window.scrollTo(0, 0);
    // The next state message contains the newly registered account.
  },
  registrationError(text) {
    pendingAdd = false;
    this.clearLogin();
    updateFolder();
    $("#form-status").textContent = text;
    if (active == null) $("#form-status").scrollIntoView({ block: "nearest" });
  },
  clearLogin() {
    $("#login-url").value = "";
    $("#login-session").hidden = true;
    $("#login-address").hidden = true;
    $("#copy-login").hidden = true;
  },
  loginLink(link) {
    if (!pendingAdd || $("#cancel-login").disabled) return;
    $("#login-url").value = link;
    $("#login-address").hidden = false;
    $("#copy-login").hidden = false;
    $("#form-status").textContent = "링크를 복사해 추가할 계정이 로그인된 브라우저 프로필의 주소창에 붙여넣으세요. 로그인을 마치면 자동으로 연결됩니다.";
    if (active == null) $("#login-session").scrollIntoView({ block: "nearest" });
  },
  notice(text) {
    $("#msg").textContent = text;
  },
};
function folderKey(path) {
  return (path || "").replaceAll("/", "\\").replace(/\\+$/, "").toLowerCase();
}
function updateFolder() {
  const provider = $("#provider").value;
  const folder = folders[provider] || defaults[provider];
  const existing = folder && accounts.find((a) => folderKey(a.folder) === folderKey(folder));
  $("#folder").textContent = folder || "폴더를 선택하세요.";
  $("#profile-hint").textContent = existing
    ? "이 폴더는 이미 ‘" + existing.alias + "’에 연결되어 있습니다. 다른 계정으로 로그인하면 새 폴더가 생깁니다."
    : "이미 로그인한 폴더를 연결하거나, 새 프로필로 로그인하세요.";
  $("#login").textContent = names[provider] + " 로그인 링크 받기";
  $("#login").classList.toggle("primary", !!existing);
  $("#attach").classList.toggle("primary", !existing);
  for (const id of ["login", "browse", "alias", "provider"]) $("#" + id).disabled = pendingAdd;
  $("#dedicated").disabled = !!existing || pendingAdd;
  $("#attach").disabled = !folder || !!existing || pendingAdd;
}
$("#provider").onchange = () => {
  $("#dedicated").checked = false;
  $("#form-status").textContent = "";
  updateFolder();
};
$("#browse").onclick = () => send("browse", { provider: $("#provider").value });
function startAdd(message) {
  if (pendingAdd || !$("#connect").reportValidity()) return false;
  pendingAdd = true;
  updateFolder();
  $("#form-status").textContent = message;
  $("#form-status").scrollIntoView({ block: "nearest" });
  return true;
}
$("#login").onclick = () => {
  if (startAdd("로그인 링크를 준비하고 있습니다…")) {
    $("#cancel-login").disabled = false;
    $("#login-session").hidden = false;
    $("#copy-login").textContent = "로그인 링크 복사";
    $("#login-session").scrollIntoView({ block: "nearest" });
    send("login", { provider: $("#provider").value, alias: $("#alias").value });
  }
};
$("#copy-login").onclick = () => {
  $("#login-url").focus();
  $("#login-url").select();
  if (document.execCommand("copy")) {
    $("#copy-login").textContent = "복사됨 · 다시 복사";
  } else {
    $("#form-status").textContent = "선택된 링크를 Ctrl+C로 복사해 원하는 브라우저에 붙여넣으세요.";
  }
};
$("#cancel-login").onclick = () => {
  $("#cancel-login").disabled = true;
  $("#login-url").value = "";
  $("#login-address").hidden = true;
  $("#copy-login").hidden = true;
  $("#form-status").textContent = "로그인을 취소하고 있습니다…";
  send("cancel_login");
};
$("#connect").onsubmit = (e) => {
  e.preventDefault();
  const provider = $("#provider").value;
  const folder = folders[provider] || defaults[provider];
  if ($("#attach").disabled || !startAdd("프로필 확인 중…")) return;
  send("add", {
    provider,
    folder,
    alias: $("#alias").value,
    dedicated: $("#dedicated").checked,
  });
};
$("#jump-add").onclick = () => {
  $("#connect").scrollIntoView({ block: "start" });
  $("#provider").focus({ preventScroll: true });
};
$("#settings").onclick = () => quota.open(null);
$("#hide").onclick = () => send("hide");
$("#quit").onclick = () => send("exit");
$("#refresh").onclick = () => {
  send(
    accounts.find((a) => a.id === active)?.locked ? "reconnect" : "refresh",
    { id: active },
  );
};
function pick(b) {
  day = Number(b.dataset.day);
  const d = accounts.find((a) => a.id === active)?.days[day];
  if (!d) return;
  document
    .querySelectorAll("[data-day]")
    .forEach((x) => x.setAttribute("aria-pressed", x === b));
  $("output").textContent = d.date + " · " + reading(d, true);
}
document.addEventListener("pointerover", (e) => {
  const b = e.target.closest("[data-day]");
  if (b) pick(b);
});
document.addEventListener("focusin", (e) => {
  if (e.target.matches("[data-day]")) pick(e.target);
});
document.addEventListener("click", (e) => {
  const b = e.target.closest("[data-tab],[data-act],[data-day]");
  if (!b) return;
  if (b.dataset.tab) {
    tab = b.dataset.tab;
    tabs();
    return;
  }
  if (b.dataset.day != null) {
    pick(b);
    return;
  }
  const id = Number(b.dataset.id);
  switch (b.dataset.act) {
    case "open":
      quota.open(id);
      break;
    case "up":
    case "down":
      send("move", { id, delta: b.dataset.act === "up" ? -1 : 1 });
      break;
    case "show":
      send("show", { id, visible: b.checked });
      break;
    case "rename": {
      const alias = prompt(
        "작업표시줄 별칭",
        accounts.find((a) => a.id === id).alias,
      );
      if (alias?.trim()) send("rename", { id, alias });
      break;
    }
    case "remove":
      if (
        confirm(
          "이 계정의 등록과 앱에 저장된 사용 이력을 삭제할까요? CLI 프로필은 그대로 유지됩니다.",
        )
      )
        send("remove", { id });
      break;
  }
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") send("hide");
});
send("ready");
