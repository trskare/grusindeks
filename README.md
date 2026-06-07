# grusindeks

CLI som beregner **Grusindeks** (0–100) for hvor optimalt det er å sykle
på grus i et gitt geografisk område — basert på prognose og observasjoner
fra [MET / yr.no](https://api.met.no).

## Hva er Grusindeks?

En vektet score 0–100 der høyere er bedre, satt sammen av fem signaler:

| Sub-score        | Vekt | Kort                                                                |
|------------------|-----:|---------------------------------------------------------------------|
| Temperatur       |  18% | **Følt** temperatur — vindkjøling under 10 °C, heat index over 27 °C |
| Vind (+ kast)    |  17% | 0–3 m/s = perfekt; gust > 1.5× snitt = straff (handling/effekt-akse) |
| Nedbør (mengde)  |  25% | 0 mm = 100; > 2 mm/h faller raskt                                   |
| Nedbør (sjanse)  |  10% | `100 − probability_of_precipitation`                                |
| Bakke            |  30% | Vannbalansemodell + flerdøgns-tørke-detektor (siste 7 døgn fra Frost) |

To **hard caps** trumfer alt: nedbør > 5 mm/h eller vind > 15 m/s
klemmer totalen ned til ≤ 25.

For hvert sjekkpunkt henter vi prognosen for senterpunktet pluss åtte
kompasspunkter (N/NØ/Ø/SØ/S/SV/V/NV) på radius (default 20 km), og
rapporterer både snitt, verste og beste punkt.

I CLI-utskriften vises de to nedbør-signalene som én **Nedbør**-rad
(vektet 25:10 — slik som i totalen).

## Installasjon

### Forutsetninger

- Rust 1.80 eller nyere for CLI-en (web-GUI-en krever 1.94+) — installer via [rustup](https://rustup.rs):
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`

Ingen andre system-avhengigheter; HTTP-klienten bruker `rustls` og
`reqwest`, så du trenger ikke OpenSSL.

### Bygg fra kilde

```sh
git clone https://github.com/trskare/grusindeks.git
cd grusindeks

# Optimalisert release-binær:
cargo build --release
# Resultatet ligger i ./target/release/grusindeks

# Eller installer globalt i ~/.cargo/bin/:
cargo install --path crates/grusindeks-cli

# Verifiser:
grusindeks --version
```

`cargo build` (uten `--release`) gir en raskere debug-bygg i
`./target/debug/grusindeks` — fint mens du eksperimenterer, men
~10× tregere ved kjøring.

## Bruk

```sh
# Første gang:
grusindeks config init                  # skriver en startmal
$EDITOR "$(grusindeks config path)"     # åpner config.toml — sett user_agent_contact

# Score for koordinater nå (3-timers vindu fra nå):
grusindeks --lat 59.9139 --lon 10.7522

# Score for et lagret sted i et bestemt vindu i dag:
grusindeks --place oslo --window 14:00-17:00

# Dag-for-dag oversikt for de neste 6 dagene (default — med konfidens,
# beste-dag-tips og evt. beste «luke» innenfor hver dag):
grusindeks --place oslo
grusindeks --place oslo --days 5

# Vis det beste 2-timers vinduet for hver dag (uavhengig av hvor mye
# bedre det er enn dagsgjennomsnittet):
grusindeks --place oslo --best-window
# Egendefinert vindu-lengde:
grusindeks --place oslo --best-window 4

# Maskinlesbart for skripting / framtidig web-API:
grusindeks --place oslo --json
grusindeks --place oslo --days 5 --json
```

### Shell-completions

CLI-en kan generere completions for `bash`, `elvish`, `fish`,
`powershell` og `zsh`:

```sh
grusindeks completions <SHELL>
```

Installer dem ved å skrive outputen til shell-ets completion-katalog:

```sh
# bash (Linux)
mkdir -p ~/.local/share/bash-completion/completions
grusindeks completions bash > ~/.local/share/bash-completion/completions/grusindeks

# zsh
mkdir -p ~/.zfunc
grusindeks completions zsh > ~/.zfunc/_grusindeks
# Legg dette i ~/.zshrc hvis du ikke allerede har det:
# fpath=(~/.zfunc $fpath)
# autoload -Uz compinit && compinit

# fish
mkdir -p ~/.config/fish/completions
grusindeks completions fish > ~/.config/fish/completions/grusindeks.fish
```

Start shell-et på nytt etter installasjon, eller source filen manuelt.

### Dag-for-dag prognose

Uten `--window` / `--hours` defaulter CLI-en til en **6-dagers
oversikt**. Hver dag scores over sykkelvinduet **06:00–22:00 lokal tid**,
og rapporten består av:

- **Tittel-linje** — `Grusindeks · {sted} · {radius} km · N dager`
- **Beste-dag-callout** i en avrundet boks (`╭─ ★ Beste: … ─╮`); fargen
  følger score-bucketet, så hele uken oppsummeres ved ett blikk.
  Konfidens-rank bryter uavgjorte slik at en `Høy`-konfidens 90 vinner
  over en `Lav` 90.
- **Uke-trend** — sparkline over alle dagene + første→siste tall +
  pil (`↗ / ↘ / →`) + spredning, så *formen* på uken er synlig.
- **Én rad per dag** med dag-etikett, vær-emoji, halv-blokks-bar og
  total-score. Lav-konfidens-dager får en `~`-markør på slutten.
- **Sub-score-breakdown** under dagens rad (i dag som default,
  `--verbose` for alle dager) som tre-grener (`├─ Temp`, `├─ Vind`,
  `├─ Nedbør`, `└─ Bakke`).
- **Per-dag `Tall`-rad** under sub-score-treet (i dag som default, alle
  dager i `--verbose`) med kompakte rå-tall: temp-spenn, total nedbør,
  maks vind og evt. kraftigste kast. Hvert tall er fargekodet etter sin
  egen sub-score (rødt = ubehagelig, grønt = behagelig), så øyet leser
  alvorlighetsgraden på tallet alene.
- **Footer-chips** — `Bakke`-tilstand når kjent (én gang for hele
  prognosen, ikke per dag), `Regn 7d` med totalt mm / våteste døgn /
  antall regndøgn fra Frost-historikken (kollapser til `tørt siste
  N døgn` når ingen dag hadde meningsfull regn — samme terskel som
  Bakke-drought-telleren), `Skala`-legenden over score-bucketene, og en
  `~`-fotnote når noen dag har lav konfidens. `Regn 7d` og `Tall` kan
  skrus av per kjøring (`--no-rain-history` / `--no-window-stats`) eller
  permanent i config (`show_rain_history` / `show_window_stats`).
- **`★ Beste luke`** (kun `--verbose`) — et 3-timers sub-vindu som
  scorer minst 10 poeng bedre enn dagen ellers. Linja avsluttes med en
  ett-ords forklaring som peker på aksen der vinduet trekker mest fra
  dagsgjennomsnittet: `tørrest`, `minst vind`, eller temperatur-grunner
  som splittes i to regimer — `mildest` når vinduets felt-temp ligger i
  16–22 °C-platået, og `minst kald` ellers.
- **`--best-window [TIMER]`** — opt-in alternativ som viser dagens
  beste sub-vindu uansett forbedring (default 2 timer). Når `work_hours`
  er aktivert i config unngås disse tidene; bruk `--include-work-hours`
  for å vise beste vindu uansett arbeidstid. Når sub-vinduet faktisk slår
  dagsgjennomsnittet brukes den vanlige `Beste luke`-merkingen med `+N poeng`-
  suffiks; ellers vises det som `Beste vindu` uten suffiks.

Konfidens faller med horisonten: `api.met.no` publiserer
time-for-time-data for de første ~60 timene; deretter kun 6-timers
oppløsning, som gir lavere konfidens og hindrer luke-deteksjon for
fjerne dager.

Eksempel-utskrift (default modus):

```
Grusindeks · Oslo · 20 km · 6 dager

╭─────────────────────────────────────────╮
│  ★ Beste: sø 26. apr  —  95  Strålende  │
╰─────────────────────────────────────────╯

  Uke   █▅█▆█▅   95 → 60   ↘   spredning 35 p

  sø 26. apr   ☀   ████████▌▒  95
  i dag        🌧   █████▍▒▒▒▒  60
      ├─ Temp      █████▒▒▒▒▒  56
      ├─ Vind      ███████▉▒▒  88
      ├─ Nedbør    ██▍▒▒▒▒▒▒▒  27
      ├─ Bakke     ███████▉▒▒  87
      └─ Tall      11–14 °C · 1.4 mm · 6 m/s (kast 9)
  i morgen     ☀   ████████▌▒  95
  on 29. apr   ☁   ██████▏▒▒▒  68
  to 30. apr   ☀   ████████▌▒  95
  fr 1. mai    🌧   █████▍▒▒▒▒  60   ~

  Bakke     tørt og løst dekke (4 døgn uten regn)
  Regn 7d   3.2 mm siste 7 døgn · våtest 22. apr (2.4 mm) · 1 regndøgn
  Skala     0 dårlig · 25 marginalt · 45 ok · 65 bra · 85 strålende
  ~         lav konfidens (>60 t — 6-t oppløsning fra MET)
```

Bar-tegnene bruker halv-blokks-glyfer (`▏▎▍▌▋▊▉█`) for at hver
prosentpoeng-endring synes — en `60` og en `62` gir tydelig forskjellig
bar selv ved bare 10 tegns bredde.

### Språk

Utskriften kommer på **norsk** som default. Sett `language = "swedish"`
i `config.toml` for å bytte til **svensk** — labels (`Strålende` →
`Strålande`, `Dårlig` → `Dåligt`), penalty-meldinger (`tørt og løst
dekke` → `torrt och löst underlag`), kompass-forkortelser (`NØ` →
`NÖ`), samt ukedager og måneder veksler alle med språket. Brand-navnet
«Grusindeks» beholdes uansett.

Samme rapport gjengitt på svensk:

```
Grusindeks · Oslo · 20 km · 6 dagar

╭─────────────────────────────────────────╮
│  ★ Bästa: sö 26. apr  —  95  Strålande  │
╰─────────────────────────────────────────╯

  Vecka █▅█▆█▅   95 → 60   ↘   spridning 35 p

  sö 26. apr   ☀   ████████▌▒  95
  idag         🌧   █████▍▒▒▒▒  60
      ├─ Temp      █████▒▒▒▒▒  56
      ├─ Vind      ███████▉▒▒  88
      ├─ Nederbörd ██▍▒▒▒▒▒▒▒  27
      ├─ Mark      ███████▉▒▒  87
      └─ Tal       11–14 °C · 1.4 mm · 6 m/s (by 9)

  Mark      torrt och löst underlag (4 dygn utan regn)
  Regn 7d   3.2 mm senaste 7 dygn · blötast 22. apr (2.4 mm) · 1 regndag
  Skala     0 dåligt · 25 marginellt · 45 ok · 65 bra · 85 strålande
  ~         låg tillförlitlighet (>60 h — 6-h upplösning från MET)
```

### Config

Stien er plattform-avhengig (`directories::ProjectDirs`):

| OS         | Sti                                                     |
| ---------- | ------------------------------------------------------- |
| Linux/BSD  | `~/.config/grusindeks/config.toml`                      |
| macOS      | `~/Library/Application Support/grusindeks/config.toml`  |
| Windows    | `%APPDATA%\grusindeks\config\config.toml`               |

Skriv `grusindeks config path` for å se hvor din ligger — eller bruk
`--config <PATH>` for å overstyre. Eksempel-innhold:

```toml
user_agent_contact = "you@example.com"  # MÅ være satt — kreves av MET TOS
default_place = "oslo"

# Valgfritt: språk i utskrift. "norwegian" (default) eller "swedish".
# language = "swedish"

# Valgfritt: skru av footer-chips. Begge defaulter til true.
# show_rain_history = false   # skjuler "Regn 7d"-linja
# show_window_stats = false   # skjuler "Tall"-linja
# (Kan også overstyres per kjøring: --no-rain-history / --no-window-stats)

# Valgfritt: arbeidstid som --best-window skal unngå.
# Overstyr én kjøring med --include-work-hours.
# [work_hours]
# enabled = true
# days = ["mon", "tue", "wed", "thu", "fri"]
# window = "08:00-15:00"

[frost]
# Registrer en gratis client_id på
# https://frost.met.no/auth/requestCredentials.html
# Uten den hopper grusindeks over historisk nedbør og antar tørr bakke.
# client_id = "00000000-0000-0000-0000-000000000000"
# source_id = "SN18700"   # Frost stasjon — f.eks. SN18700 (Blindern, Oslo)

[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
```

## Hvordan scoringen fungerer

Hvert signal mappes til en sub-score 0–100, deretter vektes de sammen
til total-poengsummen. Alle terskler ligger i én modul
(`grusindeks-core::score::thresholds`) så de er enkle å tune uten å lete
gjennom kodebasen.

```
total = (18·temp + 17·vind + 25·nedbør + 10·sjanse + 30·bakke) / 100
```

Etiketten følger totalen: 0–24 **Dårlig**, 25–44 **Marginalt**, 45–64
**OK**, 65–84 **Bra**, 85–100 **Strålende**.

### 1. Temperatur (18%) — *følt* temperatur

Aksen rangerer ikke rå lufttemperatur, men **følt-T** (apparent
temperature) for en syklist. Tre regimer:

- **Kald-siden (≤ 10 °C):** vindkjøling via NWS / Environment Canada
  2001-formelen. En syklist genererer selv ~5 m/s relativ vind ved
  gravel-tempo (18 km/t), så vindkjøling er **alltid på** når det er
  kaldt — ikke bare når det blåser i prognosen. Vi kombinerer
  ambient og selv-vind i kvadratur (`sqrt(v_ambient² + 5²)`).
- **Varm-siden (≥ 27 °C):** heat index via NWS Rothfusz-regresjonen.
  Krever luftfuktighet i prognosen; uten den faller vi tilbake til ren
  lufttemperatur så vi ikke straffer brukeren for manglende data.
- **Mellom (10–27 °C):** ingen justering — felt-T = lufttemperatur.

Den justerte verdien mappes til en sub-score: platå på 100 mellom
**16–22 °C**, knekk ved 12 °C (≈ 75 — der man begynner å trenge ekstra
lag som vest og armere, med påfølgende svette-i-motbakke /
kuldegysninger-i-utforkjøring), deretter brattere fall-off til 0 ved
−5 °C. Varm-siden faller lineært til 0 ved 35 °C.

| Lufttemp °C | Vind m/s | RH % | Felt-T °C  | Score | Kommentar                    |
|------------:|---------:|-----:|-----------:|------:|------------------------------|
| 17          | 2        | —    | 17.0       |  100  | nøytral pass-through         |
| 13          | 2        | —    | 13.0       |   81  | vest/armere — på kanten      |
|  5          | 1        | —    |  1.3       |   28  | selv-vind kjøler litt        |
|  5          | 10       | —    | −0.7       |   19  | vind morder komforten        |
|  0          | 5        | —    | −6.0       |    0  | under temp-floor (−5 °C)     |
| 28          | 2        | 60   | 29.4       |   43  | heat index begynner å bite   |
| 28          | 2        | 85   | 33.3       |   13  | tropisk fuktig               |

CLI-en viser "føles som …" i temp-penalty-meldingen når avviket er
≥ 2 °C, slik at brukeren skjønner hvorfor en mild dag scorer dårlig.

### 2. Vind (17%) — håndtering og effekt

Vinden er nå primært en *håndterings-akse* — den termiske komponenten
ligger i temperatur. Stykkevis lineær: 100 fra 0–3 m/s, ned til 50 ved
7 m/s og 12 ved 12 m/s; deretter mot 0. Kurven er strammere enn
en lineær interpolering, fordi aerodynamisk effektkostnad er kvadratisk
i (v_rider + v_wind) og 7 m/s motvind ved 25 km/t roughly **dobler**
den totale tråkkeffekten. Hvis kastvind er > 1.5× snittet, trekkes
ekstra 20 poeng (saturerer på 0).

| Snitt m/s | Gust         | Score | Kommentar                  |
|----------:|--------------|------:|----------------------------|
|       0–3 | (uansett)    |  100  | perfekt                    |
|         5 | ingen        |   75  | merkbar, men greit         |
|         7 | ingen        |   50  | grenseland                 |
|         9 | ingen        |   35  | mye vind                   |
|        12 | ingen        |   12  | tøft                       |
|         4 | 7 m/s (1.75×)|   68  | 88 − 20 (kast-straff)      |
|        15 | (uansett)    |    4  | hard-cap-territorium       |

### 3. Nedbør — mengde (25%)

Snitt nedbør (mm/h) over sykkelvinduet. 100 ved 0; faller til 60 ved
duskregn-terskel (0.5 mm/h), 20 ved tungt regn (2 mm/h), 0 ved 5+ mm/h.

| mm/h | Score |
|-----:|------:|
|  0.0 |  100  |
|  0.25|   80  |
|  0.5 |   60  |
|  1.0 |   47  |
|  2.0 |   20  |
|  5.0 |    0  |

### 4. Nedbør — sjanse (10%)

`100 − probability_of_precipitation` (i prosent). Hvis prognosen ikke
gir oss en sannsynlighet i det hele tatt, defaulter sub-scoren til 50
(nøytral) — vi vil ikke straffe brukeren for "0 % regn" når 0 % egentlig
betyr "vet ikke".

### 5. Bakke (30%) — vannbalanse + tørke

Den mest sammensatte aksen. To delmodeller jobber sammen:

**Vannbalanse-heuristikk** (kortsiktig, timer): hver simulert time legger
vi til timens nedbør og trekker fra en tørke-rate som avhenger av
temperatur, vind, sky-cover, UV og luftfuktighet. Akkumulert
overflatevann er kappet til 5 mm — grus dreneres raskt selv etter
gjennomvåtning.

**Tørke-teller** (langsiktig, døgn): vi sporer "timer siden overflaten
sist var meningsfullt fuktig" (post-tørking ≥ 0.3 mm). Lett duskregn som
bygger seg opp over flere timer nullstiller telleren; én sekund-lang
sprut som umiddelbart fordamper gjør det ikke.

Sub-scoren har optimum ved 0.4 mm akkumulert vann (lett fuktig grus
pakker seg og ruller best — bekreftet av rolling-resistance-data: Crr på
pakket fuktig grus er 0.010–0.014, mot 0.018–0.025 for løs tørr grus).
Tørr-siden taper 8 poeng (92 ved 0 mm), våt-siden faller lineært mot 0
ved 5 mm metning. Hvis tørke-telleren passerer 72 timer (3 døgn),
trekkes ytterligere opptil 15 poeng (full straff ved 168 t / 7 døgn).

| Akkumulert mm | Drought-timer | Bakke-score | Kommentar                      |
|--------------:|--------------:|------------:|--------------------------------|
|           0.0 |             0 |          92 | bone-dry, ingen langtørke      |
|           0.0 |            72 |          92 | akkurat under trigger          |
|           0.0 |           120 |          84 | midtveis i tørke-rampen        |
|           0.0 |           168 |          77 | full langtørke (max −15)       |
|           0.4 |             0 |         100 | optimum — lett fuktig          |
|           1.0 |             0 |          87 | litt våt etter regn            |
|           2.5 |             0 |          54 | godt fuktig                    |
|           5.0 |             0 |           0 | gjennomvåt                     |

Total-effekten av tørr+langtørke er bevisst beskjeden: maks ~7 poeng på
totalen (23 poeng på bakke-aksen × 30 % vekt). Våt grus er fortsatt den
klart største negative faktoren.

### Hard caps

To regler trumfer hele utregningen og klemmer totalen til ≤ 25:

- **Kraftig regn:** noen time i vinduet over 5 mm/h.
- **Stormvind:** noen time over 15 m/s.

Dette finnes for å unngå at en lineær snitt-score gir et villedende godt
resultat når én enkelt time virkelig er en deal-breaker.

### Eksempler

Eksemplene under viser **single-window-modus** (`--window 14:00-17:00`)
slik den faktisk rendres i terminalen, med formel-utregningen som
fotnote. **Nedbør**-raden er den vektede kombinasjonen av mengde og
sannsynlighet (25:10), siden de to signalene måler det samme fra to
vinkler.

**Perfekt sommerdag i Oslo.** 17 °C, 2 m/s vind, ingen nedbør, 5 %
sjanse, bakken har 0.4 mm fra lett dusk i går.

```
Grusindeks for Oslo (20km radius) — 2026-06-15 14:00–17:00
═══════════════════════════════════════════════════════════════
Total: 99/100  ⭐ Strålende

Temperatur     ██████████ 100
Vind           ██████████ 100
Nedbør         ████████▉▒  98
Bakke          ██████████ 100
```

`(18·100 + 17·100 + 25·100 + 10·95 + 30·100) / 100 = 99`. Felt-T = 17 °C
(pass-through-båndet). Ingen penalties — alle sub-scorer ligger på 80
eller høyere.

**Frisk dag med risiko for byger.** 14 °C, 6 m/s vind, ingen nedbør i
vinduet men 70 % sjanse, bakken litt våt fra i går (1.5 mm).

```
Grusindeks for Oslo (20km radius) — 2026-04-26 14:00–17:00
═══════════════════════════════════════════════════════════════
Total: 79/100  ⭐ Bra

Temperatur     ██████████ 100
Vind           █████▋▒▒▒▒  63
Nedbør         ███████▎▒▒  80
Bakke          ██████▉▒▒▒  76

  Sannsynlighet: 70 % sjanse for nedbør
  Vind: snitt 6.0 m/s
  Bakke: våt fra forrige døgn (1.5 mm)
```

`(18·100 + 17·63 + 25·100 + 10·30 + 30·76) / 100 = 79`. Vinden ligger
mellom OK (3 m/s) og POOR (7 m/s); bakken er på vei nedover den
lineære våt-rampen mot 0 ved 5 mm.

**Hard-cap-dag.** 12 °C, 4 m/s vind, men én time har 7 mm/h regn.

```
Grusindeks for Oslo (20km radius) — 2026-04-26 14:00–17:00
═══════════════════════════════════════════════════════════════
Total: 25/100  ⭐ Marginalt

Temperatur     ██████████ 100
Vind           ███████▉▒▒  88
Nedbør         █▋▒▒▒▒▒▒▒▒  18
Bakke          ████████▎▒  92

  Advarsel: Kraftig regn ventet (7.0 mm/t) — score hard-cappet
  Nedbør: 2.3 mm/t i snitt
```

Uten hard-cap ville snittet havnet rundt 67 — men når én enkelt time
har 7 mm/t er det tale om kraftig byge. Hard cap kapper totalen til
≤ 25 og merker `Advarsel`-penalty som Critical, så brukeren ikke ledes
til å tro at gjennomsnittet er trygt.

**Tørke-dag etter en uke uten regn.** 25 °C, 5 m/s, klart, 5 % sjanse,
bakken har vært bone-dry i 168 timer.

```
Grusindeks for Oslo (20km radius) — 2026-07-12 14:00–17:00
═══════════════════════════════════════════════════════════════
Total: 84/100  ⭐ Bra

Temperatur     ██████▉▒▒▒  77
Vind           ██████▊▒▒▒  75
Nedbør         ████████▉▒  98
Bakke          ██████▉▒▒▒  77

  Bakke: tørt og løst dekke (7 døgn uten regn)
  Temperatur: varmt, snitt 25 °C
  Vind: snitt 5.0 m/s
```

`(18·77 + 17·75 + 25·100 + 10·95 + 30·77) / 100 = 84`. Bakke = 92
(dry-floor) − 15 (full tørke-straff) = 77.

Den siste illustrerer designvalget: selv etter en uke uten regn er
totalen fortsatt godt over 70. Tørke er en *soft hint* via
Penalty-listen, ikke et tall som ødelegger scoren — selv om vi nå
straffer den hardere enn før (15 poeng på bakke-aksen, opp fra 10) for
å reflektere de faktiske rolling-resistance-tapene.

## Utvikling

```sh
cargo test --workspace          # ~290 tester, ingen nettverkskall
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo run -p grusindeks-cli -- --lat 59.9139 --lon 10.7522 --verbose
```

Workspace-layout:

- `crates/grusindeks-core` — domenetyper, scoring, vannbalanse, tørke,
  geo, språk-modul (ingen I/O)
- `crates/grusindeks-met` — `api.met.no` + `frost.met.no`-klienter med
  TOS-compliant User-Agent og `Expires` / `If-Modified-Since`-cache
- `crates/grusindeks-cli` — `grusindeks`-binæren

Testene bruker `wiremock` for HTTP-mock og fixtures fanget fra ekte MET
i `fixtures/`. Ingen tester treffer nettet.

## Kreditering & lisens

Værdata: © [Meteorologisk institutt (MET Norway)](https://www.met.no), via
`api.met.no` og `frost.met.no`, lisensiert under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

Denne tjenesten er ikke tilknyttet Yr eller MET.

grusindeks-koden er lisensiert under MIT eller Apache-2.0 (du velger).
