# grusindeks

CLI som beregner **Grusindeks** (0–100) for hvor optimalt det er å sykle
på grus i et gitt geografisk område — basert på prognose og observasjoner
fra [MET / yr.no](https://api.met.no).

## Hva er Grusindeks?

En vektet score 0–100 der høyere er bedre, satt sammen av fem signaler:

| Sub-score        | Vekt | Kort                                                                |
|------------------|-----:|---------------------------------------------------------------------|
| Temperatur       |  15% | Optimal 12–22 °C                                                    |
| Vind (+ kast)    |  20% | 0–3 m/s = perfekt; gust > 1.5× snitt = straff                       |
| Nedbør (mengde)  |  25% | 0 mm = 100; > 2 mm/h faller raskt                                   |
| Nedbør (sjanse)  |  10% | `100 − probability_of_precipitation`                                |
| Bakke            |  30% | Vannbalansemodell + flerdøgns-tørke-detektor (siste 7 døgn fra Frost) |

To **hard caps** trumfer alt: nedbør > 5 mm/h eller vind > 15 m/s
klemmer totalen ned til ≤ 25.

For hvert sjekkpunkt henter vi prognosen for senterpunktet pluss åtte
kompasspunkter (N/NØ/Ø/SØ/S/SV/V/NV) på radius (default 20 km), og
rapporterer både snitt, verste og beste punkt.

I CLI-utskriften vises de to nedbør-signalene som én **Nedbør**-rad
(vektet 25:10 — slik som i totalen). Bryteren `(mengde X, sjanse Y)`
dukker bare opp når de avviker mer enn 5 poeng — ellers er rommet bare
støy.

## Installasjon

### Forutsetninger

- Rust 1.80 eller nyere — installer via [rustup](https://rustup.rs):
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
grusindeks config init
$EDITOR ~/.config/grusindeks/config.toml   # sett user_agent_contact

# Score for koordinater nå (3-timers vindu fra nå):
grusindeks score --lat 59.9139 --lon 10.7522

# Score for et lagret sted i et bestemt vindu i dag:
grusindeks score --place oslo --window 14:00-17:00

# Dag-for-dag oversikt for de neste 6 dagene (default — med konfidens,
# beste-dag-tips og evt. beste «luke» innenfor hver dag):
grusindeks score --place oslo
grusindeks score --place oslo --days 5

# Maskinlesbart for skripting / framtidig web-API:
grusindeks score --place oslo --json
grusindeks score --place oslo --days 5 --json
```

### Dag-for-dag prognose

Uten `--window` / `--hours` defaulter CLI-en til en **6-dagers
oversikt**. Hver dag scores over ride-vinduet **06:00–22:00 lokal tid**,
og vi flagger:

- en **🎯 Beste dag**-headline (høyeste snitt; konfidens-rank bryter
  uavgjorte slik at en `Høy`-konfidens 90 vinner over en `Lav` 90);
- en **luke** per dag — et 3-timers sub-vindu som scorer minst 10 poeng
  bedre enn dagen ellers;
- en sub-score-breakdown under dagens rad (i dag som default,
  `--verbose` for alle dager).

Konfidens (`høy` / `middels` / `lav`) faller med horisonten:
`api.met.no` publiserer time-for-time-data for de første ~60 timene;
deretter kun 6-timers oppløsning, som gir lavere konfidens og hindrer
luke-deteksjon for fjerne dager.

Eksempel-utskrift:

```
🎯 Beste dag: to 30. apr  ·  95/100  Strålende

i dag        ⛅ █████████░  89  Strålende  ⓘ høy
             ├─ Temp    ████████░░  84
             ├─ Vind    ████████░░  80
             ├─ Nedbør  ██████████ 100
             ├─ Bakke   █████████░  86
             └─ Bakke: tørt og løst dekke (4 døgn uten regn)
i morgen     ☁  █████████░  87  Strålende  ⓘ høy
ti 28. apr   ☁  █████████░  88  Strålende  ⓘ middels
on 29. apr   ⛅ █████████░  94  Strålende  ⓘ lav
to 30. apr   ☀  ██████████  95  Strålende  ⓘ lav
```

### Config

`~/.config/grusindeks/config.toml`:

```toml
user_agent_contact = "you@example.com"  # MÅ være satt — kreves av MET TOS
default_place = "oslo"

# Valgfritt: språk i utskrift. "norwegian" (default) eller "swedish".
# language = "swedish"

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
total = (15·temp + 20·vind + 25·nedbør + 10·sjanse + 30·bakke) / 100
```

Etiketten følger totalen: 0–24 **Dårlig**, 25–44 **Marginalt**, 45–64
**OK**, 65–84 **Bra**, 85–100 **Strålende**.

### 1. Temperatur (15%)

Plateu på 100 mellom 12 °C og 22 °C; lineær fall-off til 0 ved −5 °C
(kald-siden) og 35 °C (varm-siden).

| °C   | Score | Kommentar              |
|-----:|------:|------------------------|
| −5   |    0  | for kaldt              |
|  3.5 |   50  | midtveis i kald-rampen |
| 12   |  100  | optimal                |
| 17   |  100  | optimal                |
| 22   |  100  | optimal                |
| 28.5 |   50  | midtveis i varm-rampen |
| 35   |    0  | for varmt              |

### 2. Vind (20%)

Stykkevis lineær: 100 fra 0 til 3 m/s, faller til 60 ved 7 m/s og 20
ved 12 m/s; deretter mot 0. Hvis kastvind er > 1.5× snittet, trekkes
ekstra 20 poeng (saturerer på 0).

| Snitt m/s | Gust         | Score | Kommentar                |
|----------:|--------------|------:|--------------------------|
|       0–3 | (uansett)    |  100  | perfekt                  |
|         5 | ingen        |   80  | merkbar, men greit       |
|         7 | ingen        |   60  | grenseland               |
|         9 | ingen        |   44  | mye vind                 |
|        12 | ingen        |   20  | tøft                     |
|         4 | 7 m/s (1.75×)|   60  | 80 − 20 (kast-straff)    |
|        15 | (uansett)    |   12  | hard-cap-territorium     |

### 3. Nedbør — mengde (25%)

Snitt nedbør (mm/h) over ride-vinduet. 100 ved 0; faller til 60 ved
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
pakker seg og ruller best). Tørr-siden taper maks 5 poeng (95 ved 0 mm),
våt-siden faller lineært mot 0 ved 5 mm metning. Hvis tørke-telleren
passerer 72 timer (3 døgn), trekkes ytterligere opptil 10 poeng (full
straff ved 168 t / 7 døgn).

| Akkumulert mm | Drought-timer | Bakke-score | Kommentar                      |
|--------------:|--------------:|------------:|--------------------------------|
|           0.0 |             0 |          95 | bone-dry, ingen langtørke      |
|           0.0 |            72 |          95 | akkurat under trigger          |
|           0.0 |           120 |          90 | midtveis i tørke-rampen        |
|           0.0 |           168 |          85 | full langtørke (max −10)       |
|           0.4 |             0 |         100 | optimum — lett fuktig          |
|           1.0 |             0 |          87 | litt våt etter regn            |
|           2.5 |             0 |          54 | godt fuktig                    |
|           5.0 |             0 |           0 | gjennomvåt                     |

Total-effekten av tørr+langtørke er bevisst beskjeden: maks ~5 poeng på
totalen (15 poeng på bakke-aksen × 30% vekt). Våt grus er fortsatt den
klart største negative faktoren.

### Hard caps

To regler trumfer hele utregningen og klemmer totalen til ≤ 25:

- **Kraftig regn:** noen time i vinduet over 5 mm/h.
- **Stormvind:** noen time over 15 m/s.

Dette finnes for å unngå at en lineær snitt-score gir et villedende godt
resultat når én enkelt time virkelig er en deal-breaker.

### Eksempler

**Perfekt sommerdag i Oslo.** 17 °C, 2 m/s vind, ingen nedbør, 5 %
sjanse, bakken har 0.4 mm fra lett dusk i går.

```
Temperatur     ████████████ 100
Vind           ████████████ 100
Nedbør         ████████████ 100   (mengde 100, sjanse 95)
Bakke          ████████████ 100
Total          (15·100 + 20·100 + 25·100 + 10·95 + 30·100) / 100 = 99
Label          Strålende
```

**Frisk dag med risiko for byger.** 14 °C, 6 m/s vind, ingen nedbør i
vinduet men 70 % sjanse, bakken litt våt fra i går (1.5 mm).

```
Temperatur     ████████████ 100
Vind           ████████░░░░  70   (snitt 6 m/s, mellom OK og POOR)
Nedbør (mengde)  ████████████ 100
Nedbør (sjanse)  ███░░░░░░░░░  30   (100 − 70)
Bakke          █████████░░░  76   (lineær 100→0 fra 0.4 til 5.0 mm)
Total          (15·100 + 20·70 + 25·100 + 10·30 + 30·76) / 100 = 79
Label          Bra
```

**Hard-cap-dag.** 12 °C, 4 m/s vind, men én time har 7 mm/h regn.

```
... uten hard-cap ville totalen blitt ~50
Hard cap aktivert: kraftig regn (7.0 mm/t) > 5 mm/t
Total           ≤ 25
Label           Marginalt
└─ Advarsel: Kraftig regn ventet (7.0 mm/t) — score hard-cappet
```

**Tørke-dag etter en uke uten regn.** 25 °C, 5 m/s, klart, 5 % sjanse,
bakken har vært bone-dry i 168 timer.

```
Temperatur     █████████░░░  77   (varm-rampen mellom 22 og 35 °C)
Vind           █████████░░░  80
Nedbør (mengde)  ████████████ 100
Nedbør (sjanse)  ████████████  95
Bakke          █████████░░░  85   (95 dry-floor − 10 drought)
Total          (15·77 + 20·80 + 25·100 + 10·95 + 30·85) / 100 = 87
Label          Strålende
└─ Bakke: tørt og løst dekke (7 døgn uten regn)
```

Den siste illustrerer designvalget: selv etter en uke uten regn er
totalen fortsatt over 80. Tørke er en *soft hint* via Penalty-listen,
ikke et tall som ødelegger scoren.

## Utvikling

```sh
cargo test --workspace          # ~240 tester, ingen nettverkskall
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo run -p grusindeks-cli -- score --lat 59.9139 --lon 10.7522 --verbose
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
