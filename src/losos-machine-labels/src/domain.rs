//! What a running machine's domain XML says about its network: which group
//! its label puts it in, and the host's end of each of its network cards.

/// The XML namespace pm records a machine's label and template in (pm's
/// libvirt plugin, `LABEL_NAMESPACE`).
const PM_NAMESPACE: &str = "https://losos-project.github.io/xmlns/pm-machine/1";

/// The template the LosOS boxes machine is made from (losos-boxes.nix),
/// whose machine every other one may reach.
const BOXES_TEMPLATE: &str = "losos-boxes";

/// The labels pm gives machines. Red is the untrusted one (docs/hypervisor.md).
pub const LABELS: [&str; 8] = [
    "red", "orange", "yellow", "green", "gray", "blue", "purple", "black",
];

/// Which machines a machine's cards may reach, by their set in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// A labelled machine: those with the same label, the LosOS boxes
    /// machine, the host's DNS and the networks past it.
    Label(&'static str),
    /// Red: the host's DHCP and DNS and the internet, nothing private.
    Untrusted,
    /// The LosOS boxes machine, which every other machine but a red one
    /// reaches the boxes through.
    Boxes,
    /// A machine pm did not make, such as one from virt-manager, which has
    /// no label: the others without one, as a label of its own.
    Unlabelled,
}

impl Group {
    /// The table's set its cards go in.
    pub fn set(self) -> &'static str {
        match self {
            Group::Label(label) => label,
            Group::Untrusted => "red",
            Group::Boxes => "boxes",
            Group::Unlabelled => "unlabelled",
        }
    }

    /// Whether its cards are in the trusted set, which spares them red's
    /// limits.
    pub fn trusted(self) -> bool {
        self != Group::Untrusted
    }
}

/// A running machine, as far as its network goes.
#[derive(Debug, PartialEq, Eq)]
pub struct Machine {
    pub group: Group,
    /// The host's ends of its cards, `vnet3` and the like.
    pub ports: Vec<String>,
}

/// Why a domain's XML could not be read.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error("libvirt's domain XML does not parse")]
    #[diagnostic(code(losos_machine_labels::xml))]
    Xml(#[from] roxmltree::Error),
}

/// Reads `xml`, a running domain's definition as libvirt gives its hooks.
pub fn read(xml: &str) -> Result<Machine, Error> {
    let doc = roxmltree::Document::parse(xml)?;
    let pm = doc
        .descendants()
        .find(|n| n.has_tag_name((PM_NAMESPACE, "machine")));
    let child =
        |name: &str| pm.and_then(|pm| pm.children().find(|n| n.has_tag_name((PM_NAMESPACE, name))));
    let template = child("template").and_then(|n| n.attribute("name"));
    let label = child("label")
        .and_then(|n| n.text())
        .map(str::trim)
        .and_then(|text| LABELS.iter().copied().find(|l| *l == text));
    // The boxes machine first, whatever its label: the others reach the
    // boxes through it.
    let group = match (template, label) {
        (Some(BOXES_TEMPLATE), _) => Group::Boxes,
        (_, Some("red")) => Group::Untrusted,
        (_, Some(label)) => Group::Label(label),
        // A label pm does not know is no reason to trust a machine more.
        _ if child("label").is_some() => Group::Untrusted,
        _ => Group::Unlabelled,
    };
    let ports = doc
        .descendants()
        .filter(|n| n.has_tag_name("interface"))
        .filter_map(|n| n.children().find(|c| c.has_tag_name("target")))
        .filter_map(|t| t.attribute("dev"))
        .filter(|dev| is_port(dev))
        .map(str::to_owned)
        .collect();
    Ok(Machine { group, ports })
}

/// Whether `name` can be a network interface's name, and so goes into an
/// nft script quoted without anything in it to escape.
pub fn is_port(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 16
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(metadata: &str, devices: &str) -> String {
        format!(
            "<domain type='kvm'><name>m</name><metadata>{metadata}</metadata><devices>{devices}</devices></domain>"
        )
    }

    const CARD: &str = "<interface type='network'><mac address='52:54:00:12:34:56'/><source network='default' bridge='virbr0'/><target dev='vnet3'/><model type='virtio'/></interface>";

    #[test]
    fn reads_the_label_and_the_ports() {
        let xml = domain(
            &format!(
                "<pm:machine xmlns:pm='{PM_NAMESPACE}'><pm:label>blue</pm:label><pm:template name='losos' digest='d'/></pm:machine>"
            ),
            &format!("{CARD}<interface type='network'><target dev='vnet4'/></interface>"),
        );
        assert_eq!(
            read(&xml).unwrap(),
            Machine {
                group: Group::Label("blue"),
                ports: vec!["vnet3".into(), "vnet4".into()],
            }
        );
    }

    #[test]
    fn red_is_untrusted_and_any_prefix_will_do() {
        let xml = domain(
            &format!("<x:machine xmlns:x='{PM_NAMESPACE}'><x:label> red </x:label></x:machine>"),
            CARD,
        );
        assert_eq!(read(&xml).unwrap().group, Group::Untrusted);
    }

    #[test]
    fn the_boxes_machine_by_its_template() {
        let xml = domain(
            &format!(
                "<pm:machine xmlns:pm='{PM_NAMESPACE}'><pm:label>green</pm:label><pm:template name='losos-boxes' digest='d'/></pm:machine>"
            ),
            CARD,
        );
        assert_eq!(read(&xml).unwrap().group, Group::Boxes);
    }

    #[test]
    fn no_label_and_unknown_labels() {
        assert_eq!(read(&domain("", CARD)).unwrap().group, Group::Unlabelled);
        // Another namespace's label is not pm's.
        let other = domain(
            "<o:machine xmlns:o='urn:other'><o:label>blue</o:label></o:machine>",
            CARD,
        );
        assert_eq!(read(&other).unwrap().group, Group::Unlabelled);
        let unknown = domain(
            &format!(
                "<pm:machine xmlns:pm='{PM_NAMESPACE}'><pm:label>teal</pm:label></pm:machine>"
            ),
            CARD,
        );
        assert_eq!(read(&unknown).unwrap().group, Group::Untrusted);
    }

    #[test]
    fn ports_that_could_inject_are_left_out() {
        let xml = domain(
            "",
            "<interface type='network'><target dev='vnet0 } ; flush ruleset'/></interface><interface type='user'/>",
        );
        assert!(read(&xml).unwrap().ports.is_empty());
        assert!(is_port("vnet12"));
        assert!(!is_port("a-very-long-name0"));
        assert!(!is_port("\"vnet\""));
    }
}
