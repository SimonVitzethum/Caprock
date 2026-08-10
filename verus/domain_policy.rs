// Caprock — Verus-Pilot (Tier 2), Teil 4: Domaenen-Policy (`domain_audit`, ext-22).
//
// Formale Spezifikation der Cap-Typ-pro-Domaene-Policy (crates/caprock-microkit/src/lib.rs,
// `domain_audit` Codes 1+2): Hardware-Caps (MMIO/IRQ/DMA) duerfen NUR HardwareLand-PDs halten,
// Autoritaets-Caps (PdControl/Loader) NUR TrustedSas-PDs. Bewiesen: das Gate `install_cap_checked`
// **erhaelt** diese Policy-Invariante (eine HW-Cap landet nie in einer Nicht-HardwareLand-PD, eine
// PdControl-Cap nie ausserhalb TrustedSas) -- statisch + fuer ALLE Zustaende.
//
// Ein ANDERER Invariantentyp als die CDT-Beweise: ein Policy-Gate erhaelt eine Klassifikations-
// Invariante (statt einer verketteten Datenstruktur).
//
// Lauf:  tools/verus-verify.sh
use vstd::prelude::*;

verus! {

/// Sicherheitsdomaene einer PD (unveraenderlicher Tag, Modell von `Domain`).
pub enum Domain {
    TrustedSas,
    HardwareLand,
    UserLand,
}

/// Cap-Typ-Klasse (abstrahiert `ObjectKind`): Hardware = MMIO/IRQ/DMA; Authority = PdControl/Loader;
/// Other = alles uebrige (ueberall erlaubt).
pub enum Kind {
    Hardware,
    Authority,
    Other,
}

/// Darf eine PD der Domaene `d` eine Cap der Klasse `k` halten? (Spiegel von `domain_allows_kind`.)
pub open spec fn domain_allows_kind(d: Domain, k: Kind) -> bool {
    match k {
        Kind::Hardware => d == Domain::HardwareLand,
        Kind::Authority => d == Domain::TrustedSas,
        Kind::Other => true,
    }
}

/// Eine Protection Domain: ihre Domaene + die gehaltenen Cap-Klassen.
pub struct Pd {
    pub domain: Domain,
    pub caps: Seq<Kind>,
}

/// Das System: eine Menge von PDs.
pub struct System {
    pub pds: Seq<Pd>,
}

/// **Domaenen-Policy-Invariante (`domain_audit` Codes 1+2):** JEDE von einer PD gehaltene Cap
/// erfuellt die Domaenen-Policy.
pub open spec fn policy_inv(s: System) -> bool {
    forall|p: int, c: int|
        #![trigger s.pds[p].caps[c]]
        0 <= p < s.pds.len() && 0 <= c < s.pds[p].caps.len()
            ==> domain_allows_kind(s.pds[p].domain, s.pds[p].caps[c])
}

/// **BEWEIS:** `install_cap_checked` (eine Cap der Klasse `k` in PD `p` installieren — **nur**, wenn
/// die Domaenen-Policy es erlaubt; sonst Ablehnung ohne Aenderung) **erhaelt** die Policy-Invariante.
pub proof fn install_cap_checked(s: System, p: int, k: Kind) -> (s2: System)
    requires
        policy_inv(s),
        0 <= p < s.pds.len(),
    ensures
        policy_inv(s2),
{
    if domain_allows_kind(s.pds[p].domain, k) {
        let pd = s.pds[p];
        let pd2 = Pd { domain: pd.domain, caps: pd.caps.push(k) };
        let s2 = System { pds: s.pds.update(p, pd2) };
        // policy_inv(s2): PD p != x unveraendert; in PD p sind die alten Caps unveraendert + die
        // neue Cap k erfuellt die Policy (Branch-Bedingung), bei gleicher Domaene.
        assert forall|x: int, cc: int| 0 <= x < s2.pds.len() && 0 <= cc < s2.pds[x].caps.len()
            implies domain_allows_kind(s2.pds[x].domain, #[trigger] s2.pds[x].caps[cc]) by {
            if x == p {
                if cc < pd.caps.len() {
                    assert(s2.pds[x].caps[cc] == pd.caps[cc]);
                }
            } else {
                assert(s2.pds[x] == s.pds[x]);
            }
        }
        s2
    } else {
        // Policy verletzt -> abgelehnt, System unveraendert.
        s
    }
}

fn main() {}

} // verus!
