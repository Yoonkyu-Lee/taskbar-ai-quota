// All times belong to the frozen preview clock (November 26, 10:24).
const sampleDay = new Date(2026, 10, 26, 10, 24);
const accounts = [
  {id:"c1", provider:"claude", alias:"개인", plan:"Pro", email:"min@example.com", short:72, week:48, fable:83, reset:[196,2556], tickets:null},
  {id:"c2", provider:"claude", alias:"업무", plan:"Team", email:"studio@example.com", short:28, week:3, fable:66, reset:[46,2520], tickets:null},
  {id:"c3", provider:"claude", alias:"연구", plan:"Max", email:"lab@example.com", short:93, week:84, reset:[236,3240], tickets:null},
  {id:"g1", provider:"openai", alias:"개인", plan:"Plus", email:"min@example.com", short:86, week:92, reset:[221,5900], tickets:2, expires:"12월 3일 10:24"},
  {id:"g2", provider:"openai", alias:"업무", plan:"Business", email:"studio@example.com", short:54, week:37, reset:[146,1356], tickets:0},
  {id:"g3", provider:"openai", alias:"연구", plan:"Pro", email:"lab@example.com", short:67, week:73, reset:[56,4200], tickets:1, expires:"11월 30일 10:24"}
];
const providers = {claude:"Claude", openai:"GPT"};
const stateKey = "quota.ring-study.1";
let saved = {};
try { saved = JSON.parse(localStorage.getItem(stateKey)) || {}; } catch {}
for (const a of accounts) {
  const prefs = saved[a.id] || {};
  a.added = a.alias !== "연구" || prefs.added === true;
  a.visible = a.added && prefs.visible !== false;
  a.redeemed = a.tickets > 0 && prefs.redeemed === true;
}
let activeId = "c1";
let selectedDay = 29;
let view = location.hash === "#history" ? "history" : "limits";
const $ = selector => document.querySelector(selector);
function save(){
  const prefs = {};
  for(const {id, added, visible, redeemed} of accounts) prefs[id] = {added,visible,redeemed};
  try {localStorage.setItem(stateKey, JSON.stringify(prefs));} catch {}
}
function used(a, windowName){
  return windowName === "short" && a.redeemed ? 0 : 100 - a[windowName];
}
function logo(provider){
  return '<img class="logo" src="'+provider+'.svg" alt="">';
}
function clock(minutes){
  const d = new Date(sampleDay.getTime() + minutes * 60000);
  return d.toTimeString().slice(0,5);
}
function resetLabel(minutes){
  const end = new Date(sampleDay.getTime() + minutes * 60000);
  const day = Math.floor(minutes/1440), hour = Math.floor(minutes%1440/60), min = minutes%60;
  const left = [day && day+"일",hour && hour+"시간",min && min+"분"].filter(Boolean).join(" ");
  return left+" 후 초기화 · "+(end.getMonth()+1)+"/"+end.getDate()+" "+clock(minutes);
}
const faces = { week: "주간", fable: "Fable 주간", short: "5시간" };

function gauge(a){
  const windows = Object.keys(faces).filter(key => a[key] != null);
  const usage = Object.fromEntries(windows.map(key => [key, used(a,key)]));
  const label = windows.map(key=>faces[key]+" "+usage[key]+"%").join(", ");
  const description = providers[a.provider]+" "+a.alias+" · "+a.email+" · 사용률 "+label;
  const resets = { week: a.reset[1] / (7 * 1440), short: a.redeemed ? 1 : a.reset[0] / 300 };
  return `<button class="gauge-btn ${a.provider}" data-open="${a.id}" aria-expanded="${activeId===a.id}" aria-controls="panel" aria-label="${description}" title="${description}">
    <span class="gauge">${dialPreview(usage, a.provider, resets)}</span><span class="account-name">${a.alias}</span></button>`;
}
function drawGauges(){
  const study=[], tray=[];
  for(const [id,name] of Object.entries(providers)){
    const list=accounts.filter(a=>a.provider===id && a.visible);
    if(!list.length) continue;
    study.push(`<div class="group"><h3>${name}</h3><div class="row">${list.map(gauge).join("")}</div></div>`);
    tray.push(`<div class="tray-group">${list.map(gauge).join("")}</div>`);
  }
  $("#study").innerHTML = study.join("") || '<span class="empty">아래에서 표시할 계정을 선택하세요.</span>';
  $("#actual").innerHTML = tray.join("");
  $("#dim").textContent = tray.length ? $("#actual").scrollWidth+" × "+$("#actual").offsetHeight+" px" : "";
}
function drawCards(){
  $("#cards").innerHTML=Object.entries(providers).map(([id,name])=>{
    const list=accounts.filter(a=>a.provider===id && a.added);
    const full=list.length===accounts.filter(a=>a.provider===id).length;
    const rows=list.map(a=>`<div class="account"><button class="profile" data-open="${a.id}" aria-controls="panel"><b>${a.alias}</b> <span>· ${a.plan}</span><small>${a.email}</small></button>
      <button class="switch" data-toggle="${a.id}" role="switch" aria-label="${name} ${a.alias} 작업표시줄에 표시" aria-checked="${a.visible}"><i></i></button></div>`).join("");
    return `<section class="card ${id}"><div class="cardhead">${logo(id)}<h3>${name}</h3><button class="add" data-add="${id}" ${full?"disabled":""}>${full?"예시 계정 3개":"+ 예시 계정"}</button></div>${rows}</section>`;
  }).join("");
}
function limitRow(key, value, minutes){
  return `<div class="quota ${key}"><div class="quota-head"><span>${faces[key]} 한도</span><b>${value}% 사용</b></div><div class="meter"><i style="width:${value}%"></i></div><div class="meta">${resetLabel(minutes)}</div></div>`;
}
function limits(a){
  const count=a.tickets===null?null:a.tickets-Number(a.redeemed);
  let creditNote="이 계정에 제공된 리셋권 정보가 없습니다.";
  if(count!==null) creditNote=count ? "만료 "+a.expires : "사용 가능한 리셋권이 없습니다.";
  const canUse=count>0 && used(a,"short")>0;
  return limitRow("week",used(a,"week"),a.reset[1])+
    (a.fable != null ? limitRow("fable",used(a,"fable"),a.reset[1]) : "")+
    limitRow("short",used(a,"short"),a.redeemed?300:a.reset[0])+
    `<div class="ticket"><span>리셋권</span><div><b>${count===null?"정보 없음":count+"장"}</b>${canUse?'<button class="action" id="redeem" title="예시 리셋권 1장으로 5시간 한도 초기화">5시간 리셋 체험</button>':""}</div></div>
    <p class="feedback">${creditNote}${a.redeemed?" · 5시간 리셋 체험 완료":""}</p>`;
}
// Daily fixture rows run from Oct 28 through Nov 26. Each pair contains
// thousands of tokens and estimated US cents. A missing day has no log records.
const dailyUsage = {
  c1: [[123,49],[280,102],null,[408,161],[192,79],[75,35],null,[310,124],[610,254],[480,193],[210,85],null,[135,56],[340,133],[810,358],[465,190],[277,128],[508,205],[690,283],null,[184,76],[279,109],[420,182],[792,349],[650,271],[115,52],null,[408,183],[650,292],[420,185]],
  c2: [[420,271],null,[760,510],[190,130],[680,422],null,[700,473],[812,546],null,[320,228],[695,496],[410,297],[476,351],null,[610,451],[480,363],[950,711],[780,596],[821,614],null,null,[606,448],[705,598],[860,636],[490,326],[730,524],null,[610,444],[799,575],[328,239]],
  c3: [null,[12,8],[46,24]],
  g1: [[130,52],null,[80,34],[96,39],[420,156],[130,52],null,[120,51],[178,71],[400,162],null,[222,87],[287,116],[310,119],[105,44],null,[310,111],[596,218],[190,69],[168,66],null,[460,172],[299,119],[398,148],[100,44],null,[250,96],[318,120],[410,144],[280,98]],
  g2: [[412,152],[226,89],null,[459,185],[697,262],null,[369,129],[855,342],[340,137],null,[580,201],[612,243],[267,104],null,[305,121],[474,187],[288,108],[514,201],null,[680,264],[815,331],[476,181],null,[672,267],[410,162],[520,191],null,[604,249],[720,292],null],
  g3: []
};
const exactTokens = new Intl.NumberFormat("ko-KR");
const compactTokens = new Intl.NumberFormat("en-US",{notation:"compact",maximumFractionDigits:2});
const usd = new Intl.NumberFormat("en-US",{style:"currency",currency:"USD"});

function history(a){
  const records = dailyUsage[a.id];
  const days = Array(30-records.length).fill(null).concat(records);
  const max = Math.max(0,...days.map(day=>day?.[0] ?? 0));
  const source = a.provider === "claude" ? "Claude Code" : "Codex";
  const sourceNote = '<p class="meta">'+source+' 로컬 로그 형식의 예시 · API 환산 추정 비용</p>';
  if(!max) return sourceNote+'<p class="no-history">최근 30일간 기록된 사용량이 없습니다.</p>';

  const bars = days.map((day,index)=>{
    const date = new Date(sampleDay);
    date.setDate(date.getDate()-29+index);
    const label = (date.getMonth()+1)+"/"+date.getDate();
    const reading = day ? exactTokens.format(day[0]*1000)+" 토큰 · "+usd.format(day[1]/100) : "기록 없음";
    const detail = label+" · "+reading;
    return `<button class="day" data-day="${index}" title="${detail}" aria-label="${detail}" aria-pressed="${index===selectedDay}">
      <i style="height:${100*(day?.[0] ?? 0)/max}%"></i></button>`;
  }).join("");
  const recorded = days.filter(Boolean);
  const total = recorded.reduce(([k,cents],day)=>[k+day[0],cents+day[1]],[0,0]);
  const periods = [["오늘",days[29]],["어제",days[28]],["최근 30일",total]];
  const summary = periods.map(([label,day])=>{
    const reading = day ? usd.format(day[1]/100)+" · "+compactTokens.format(day[0]*1000)+" 토큰" : "기록 없음";
    const exact = day ? exactTokens.format(day[0]*1000)+" 토큰" : reading;
    return '<div><dt>'+label+'</dt><dd title="'+exact+'">'+reading+'</dd></div>';
  }).join("");
  return sourceNote+`<section class="usage-trend" aria-label="최근 30일 토큰 사용량">
    <div class="history-head"><h3>일별 토큰 사용량</h3><span class="meta">최근 30일</span></div>
    <div class="day-chart">${bars}</div><div class="date-axis meta"><span>10/28</span><span>11/26</span></div>
    <output id="day-reading" class="meta"></output></section><dl class="usage-periods">${summary}</dl>`;
}
function selectDay(button){
  selectedDay=Number(button.dataset.day);
  document.querySelectorAll(".day").forEach(day=>day.setAttribute("aria-pressed",day===button));
  $("#day-reading").textContent=button.title;
}
function drawPanel(){
  const panel=$("#panel"),a=accounts.find(item=>item.id===activeId);
  panel.hidden=!a;
  if(!a){panel.replaceChildren();return;}
  panel.className=a.provider;
  panel.innerHTML=`<div class="panelhead">${logo(a.provider)}<div class="identity"><h2 id="account-title">${providers[a.provider]} <span>· ${a.alias}</span></h2><p>${a.plan} 구독 · ${a.email}</p></div><button class="close" aria-label="상세 닫기">×</button></div>
    <div class="tabs" role="group" aria-label="계정 상세 보기"><button data-view="limits" aria-pressed="${view==="limits"}">할당량</button><button data-view="history" aria-pressed="${view==="history"}">사용 이력</button></div>
    <div class="tab-pages">
      <div data-page="limits" ${view==="limits"?"":"inert"}>${limits(a)}</div>
      <div data-page="history" ${view==="history"?"":"inert"}>${history(a)}</div>
    </div>`;
  const day=$("#panel .day[aria-pressed=true]");
  if(day) selectDay(day);
}
function draw(){
  drawCards(); drawPanel(); drawGauges();
}
let openerGroup="#study";
function closePanel(){
  const id=activeId; activeId=null; drawPanel(); drawGauges();
  $(openerGroup+' [data-open="'+id+'"]')?.focus();
}
document.addEventListener("click",e=>{
  const b=e.target.closest("button");
  if(!b)return;
  if(b.dataset.open){
    openerGroup="#"+b.closest("#study,#actual,#cards").id;
    activeId=b.dataset.open; selectedDay=29; view="limits"; drawPanel();drawGauges();$("#panel .close").focus({preventScroll:true});
    if(matchMedia("(max-width:740px)").matches)$("#panel").scrollIntoView({block:"nearest"});
  }else if(b.matches(".close")){
    closePanel();
  }else if(b.dataset.view){
    view=b.dataset.view;
    document.querySelectorAll("#panel [data-view]").forEach(tab=>tab.setAttribute("aria-pressed",tab===b));
    document.querySelectorAll("#panel [data-page]").forEach(page=>page.inert=page.dataset.page!==view);
  }else if(b.hasAttribute("data-day")){
    selectDay(b);
  }else if(b.dataset.toggle){
    const a=accounts.find(item=>item.id===b.dataset.toggle);
    a.visible=!a.visible;save();drawGauges();b.setAttribute("aria-checked",a.visible);
  }else if(b.dataset.add){
    const a=accounts.find(item=>item.provider===b.dataset.add && !item.added);
    if(a){a.added=true;a.visible=true;save();draw();$('[data-toggle="'+a.id+'"]').focus();}
  }else if(b.id==="theme"){
    const light=$(".stage").classList.toggle("light");
    b.setAttribute("aria-pressed",light);b.textContent=light?"어두운 배경":"밝은 배경";
  }else if(b.id==="redeem"){
    const a=accounts.find(item=>item.id===activeId);
    if(a && a.tickets>0 && !a.redeemed){
      a.redeemed=true;save();drawPanel();drawGauges();$('[data-view="limits"]').focus();
    }
  }
});
$("#panel").addEventListener("pointerover",e=>{const day=e.target.closest(".day");if(day)selectDay(day)});
$("#panel").addEventListener("focusin",e=>{if(e.target.matches(".day"))selectDay(e.target)});
document.addEventListener("keydown",e=>{if(e.key==="Escape" && activeId)closePanel()});
draw();
window.addEventListener("resize",drawGauges);


