# medvind

Beregner **Grusindeks** (0–100) for hvor optimalt det er å sykle på grus
i et gitt geografisk område — basert på prognose og observasjoner fra
[MET / yr.no](https://api.met.no).

> _medvind_ (no.) — vind i ryggen, gunstige forhold.

## Hva er Grusindeks?

En vektet score 0–100 der høyere er bedre, satt sammen av fem signaler:

| Sub-score        | Vekt | Kort                                                               |
|------------------|-----:|--------------------------------------------------------------------|
| Temperatur       |  15% | Optimal 12–22 °C                                                   |
| Vind (+ kast)    |  20% | 0–3 m/s = perfekt; gust > 1.5× snitt = straff                      |
| Nedbør i vinduet |  25% | 0 mm = 100; > 2 mm/h faller raskt                                  |
| Regnsannsynl.    |  10% | `100 − probability_of_precipitation`                               |
| Bakke-fuktighet  |  30% | Vannbalansemodell over siste 48t observasjoner (Frost) + prognose  |

To **hard caps** trumfer alt: nedbør > 5 mm/h eller vind > 15 m/s
klemmer totalen ned til ≤ 25.

For hvert sjekkpunkt henter vi prognosen for senterpunktet pluss åtte
kompasspunkter (N/NØ/Ø/SØ/S/SV/V/NV) på radius (default 20 km), og
rapporterer både snitt, verste og beste punkt.

## Bruk

```sh
# Første gang:
medvind config init
$EDITOR ~/.config/medvind/config.toml   # sett user_agent_contact

# Score for koordinater nå (3-timers vindu fra nå):
medvind score --lat 59.9139 --lon 10.7522

# Score for et lagret sted i et bestemt vindu i dag:
medvind score --place oslo --window 14:00-17:00

# Maskinlesbart for skripting / framtidig web-API:
medvind score --place oslo --json
```

### Config

`~/.config/medvind/config.toml`:

```toml
user_agent_contact = "you@example.com"  # MÅ være satt — kreves av MET TOS
default_place = "oslo"

[frost]
# Registrer en gratis client_id på
# https://frost.met.no/auth/requestCredentials.html
# Uten den hopper medvind over historisk nedbør og antar tørr bakke.
# client_id = "00000000-0000-0000-0000-000000000000"
# source_id = "SN18700"   # Frost stasjon — f.eks. SN18700 (Blindern, Oslo)

[places.oslo]
lat = 59.9139
lon = 10.7522
radius_km = 20.0
```

## Utvikling

```sh
cargo test --workspace          # 150 tester, ingen nettverkskall
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo run -p medvind-cli -- score --lat 59.9139 --lon 10.7522 --verbose
```

Workspace-layout:

- `crates/medvind-core` — domenetyper, scoring, vannbalanse, geo (ingen I/O)
- `crates/medvind-met` — `api.met.no` + `frost.met.no`-klienter med
  TOS-compliant User-Agent og `Expires` / `If-Modified-Since`-cache
- `crates/medvind-cli` — `medvind`-binæren

Testene bruker `wiremock` for HTTP-mock og fixtures fanget fra ekte MET
i `fixtures/`. Ingen tester treffer nettet.

## Kreditering & lisens

Værdata: © [Meteorologisk institutt (MET Norway)](https://www.met.no), via
`api.met.no` og `frost.met.no`, lisensiert under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

Denne tjenesten er ikke tilknyttet Yr eller MET.

medvind-koden er lisensiert under MIT eller Apache-2.0 (du velger).
