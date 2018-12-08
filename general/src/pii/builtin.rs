use std::collections::BTreeMap;

use lazy_static::lazy_static;

use crate::pii::{
    AliasRule, AliasSelector, HashAlgorithm, HashRedaction, KindSelector, MaskRedaction,
    RedactPairRule, Redaction, ReplaceRedaction, RuleSpec, RuleType, SelectorType,
};
use crate::processor::PiiKind;

pub static BUILTIN_SELECTORS: &[&'static str] = &["text", "container"];

lazy_static! {
    #[rustfmt::skip]
    pub(crate) static ref BUILTIN_SELECTORS_MAP: BTreeMap<&'static str, SelectorType> = {
        let mut map = BTreeMap::new();
        map.insert("text", SelectorType::Kind(KindSelector { kind: PiiKind::Text }));
        map.insert("container", SelectorType::Kind(KindSelector { kind: PiiKind::Container }));

        // These are legacy aliases for compatibility
        map.insert("freeform", SelectorType::Alias(AliasSelector { selector: "text".to_string() }));
        map.insert("databag", SelectorType::Alias(AliasSelector { selector: "container".to_string() }));
        map
    };
}

macro_rules! declare_builtin_rules {
    ($($rule_id:expr => $spec:expr;)*) => {
        lazy_static! {
            pub(crate) static ref BUILTIN_RULES_MAP: BTreeMap<&'static str, &'static RuleSpec> = {
                let mut map = BTreeMap::new();
                $(
                    map.insert($rule_id, Box::leak(Box::new($spec)) as &'static _);
                )*
                map
            };
        }

        /// Names of all builtin rules
        pub static BUILTIN_RULES: &[&'static str] = &[
            $($rule_id,)*
        ];
    }
}

macro_rules! rule_alias {
    ($target:expr) => {
        RuleSpec {
            ty: RuleType::Alias(AliasRule {
                rule: ($target).into(),
                hide_inner: true,
            }),
            redaction: Redaction::Default,
        }
    };
}

declare_builtin_rules! {
    // anything
    "@anything" => rule_alias!("@anything:replace");
    "@anything:replace" => RuleSpec {
        ty: RuleType::Anything,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[redacted]".into(),
        }),
    };
    "@anything:hash" => RuleSpec {
        ty: RuleType::Anything,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // ip rules
    "@ip" => rule_alias!("@ip:replace");
    "@ip:replace" => RuleSpec {
        ty: RuleType::Ip,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[ip]".into(),
        }),
    };
    "@ip:hash" => RuleSpec {
        ty: RuleType::Ip,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // imei rules
    "@imei" => rule_alias!("@imei:replace");
    "@imei:replace" => RuleSpec {
        ty: RuleType::Imei,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[imei]".into(),
        }),
    };
    "@imei:hash" => RuleSpec {
        ty: RuleType::Imei,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // mac rules
    "@mac" => rule_alias!("@mac:mask");
    "@mac:replace" => RuleSpec {
        ty: RuleType::Mac,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[mac]".into(),
        }),
    };
    "@mac:mask" => RuleSpec {
        ty: RuleType::Mac,
        redaction: Redaction::Mask(MaskRedaction {
            mask_char: '*',
            chars_to_ignore: "-:".into(),
            range: (Some(9), None),
        }),
    };
    "@mac:hash" => RuleSpec {
        ty: RuleType::Mac,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // email rules
    "@email" => rule_alias!("@email:replace");
    "@email:mask" => RuleSpec {
        ty: RuleType::Email,
        redaction: Redaction::Mask(MaskRedaction {
            mask_char: '*',
            chars_to_ignore: ".@".into(),
            range: (None, None),
        }),
    };
    "@email:replace" => RuleSpec {
        ty: RuleType::Email,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[email]".into(),
        }),
    };
    "@email:hash" => RuleSpec {
        ty: RuleType::Email,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // creditcard rules
    "@creditcard" => rule_alias!("@creditcard:mask");
    "@creditcard:mask" => RuleSpec {
        ty: RuleType::Creditcard,
        redaction: Redaction::Mask(MaskRedaction {
            mask_char: '*',
            chars_to_ignore: " -".into(),
            range: (None, Some(-4)),
        }),
    };
    "@creditcard:replace" => RuleSpec {
        ty: RuleType::Creditcard,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[creditcard]".into(),
        }),
    };
    "@creditcard:hash" => RuleSpec {
        ty: RuleType::Creditcard,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // pem rules
    "@pemkey" => rule_alias!("@pemkey:replace");
    "@pemkey:replace" => RuleSpec {
        ty: RuleType::Pemkey,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[pemkey]".into(),
        }),
    };
    "@pemkey:hash" => RuleSpec {
        ty: RuleType::Pemkey,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // url secrets
    "@urlauth" => rule_alias!("@urlauth:replace");
    "@urlauth:replace" => RuleSpec {
        ty: RuleType::UrlAuth,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[auth]".into(),
        }),
    };
    "@urlauth:hash" => RuleSpec {
        ty: RuleType::UrlAuth,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // user path rules
    "@userpath" => rule_alias!("@userpath:replace");
    "@userpath:replace" => RuleSpec {
        ty: RuleType::Userpath,
        redaction: Redaction::Replace(ReplaceRedaction {
            text: "[user]".into(),
        }),
    };
    "@userpath:hash" => RuleSpec {
        ty: RuleType::Userpath,
        redaction: Redaction::Hash(HashRedaction {
            algorithm: HashAlgorithm::HmacSha1,
            key: None,
        }),
    };

    // password field removal
    "@password" => rule_alias!("@password:remove");
    "@password:remove" => RuleSpec {
        ty: RuleType::RedactPair(RedactPairRule {
            key_pattern: "(?i)\\b(password|passwd|mysql_pwd|auth|credentials|secret)\\b".into(),
        }),
        redaction: Redaction::Remove,
    };
}

// TODO: Move these tests to /tests
#[cfg(test)]
mod tests {
    use crate::pii::config::PiiConfig;
    use crate::pii::processor::PiiProcessor;
    use crate::processor::{process_value, ProcessingState};
    use crate::types::{Annotated, Remark, RemarkType};
    use std::collections::BTreeMap;

    #[derive(ToValue, FromValue, ProcessValue, Debug, Clone, PartialEq)]
    struct FreeformRoot {
        #[metastructure(pii = "true")]
        value: Annotated<String>,
    }

    macro_rules! assert_text_rule {
        (
            rule = $rule:expr; input = $input:expr; output = $output:expr; remarks = $remarks:expr;
        ) => {{
            let config = PiiConfig {
                rules: Default::default(),
                selectors: Default::default(),
                vars: Default::default(),
                applications: {
                    let mut map = BTreeMap::new();
                    map.insert("text".to_string(), vec![$rule.to_string()]);
                    map
                },
            };
            let input = $input.to_string();
            let mut processor = PiiProcessor::new(&config);
            let mut root = Annotated::new(FreeformRoot {
                value: Annotated::new(input),
            });
            process_value(&mut root, &mut processor, ProcessingState::default());
            let root = root.0.unwrap();
            assert_eq_str!(root.value.value().unwrap(), $output);
            let remarks = $remarks;
            assert_eq_dbg!(
                root.value.meta().iter_remarks().collect::<Vec<_>>(),
                remarks.iter().collect::<Vec<_>>()
            );
        }};
    }

    #[test]
    fn test_anything() {
        assert_text_rule!(
            rule = "@anything";
            input = "before 127.0.0.1 after";
            output = "[redacted]";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@anything", (0, 10)),
            ];
        );
        assert_text_rule!(
            rule = "@anything:replace";
            input = "before 127.0.0.1 after";
            output = "[redacted]";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@anything:replace", (0, 10)),
            ];
        );
        assert_text_rule!(
            rule = "@anything:hash";
            input = "before 127.0.0.1 after";
            output = "3D8FF1CECA9B899D532AA6679E952801DF9E5C74";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@anything:hash", (0, 40)),
            ];
        );
    }

    #[test]
    fn test_ipv4() {
        assert_text_rule!(
            rule = "@ip";
            input = "before 127.0.0.1 after";
            output = "before [ip] after";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@ip", (7, 11)),
            ];
        );
        assert_text_rule!(
            rule = "@ip:replace";
            input = "before 127.0.0.1 after";
            output = "before [ip] after";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@ip:replace", (7, 11)),
            ];
        );
        assert_text_rule!(
            rule = "@ip:hash";
            input = "before 127.0.0.1 after";
            output = "before AE12FE3B5F129B5CC4CDD2B136B7B7947C4D2741 after";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@ip:hash", (7, 47)),
            ];
        );
    }

    #[test]
    fn test_ipv6() {
        assert_text_rule!(
            rule = "@ip";
            input = "before ::1 after";
            output = "before [ip] after";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@ip", (7, 11)),
            ];
        );
        assert_text_rule!(
            rule = "@ip:replace";
            input = "[2001:0db8:85a3:0000:0000:8a2e:0370:7334]";
            output = "[[ip]]";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@ip:replace", (1, 5)),
            ];
        );
        assert_text_rule!(
            rule = "@ip:hash";
            input = "before 2001:0db8:85a3:0000:0000:8a2e:0370:7334 after";
            output = "before 8C3DC9BEED9ADE493670547E24E4E45EDE69FF03 after";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@ip:hash", (7, 47)),
            ];
        );
        assert_text_rule!(
            rule = "@ip";
            input = "foo::1";
            output = "foo::1";
            remarks = vec![];
        );
    }

    #[test]
    fn test_imei() {
        assert_text_rule!(
            rule = "@imei";
            input = "before 356938035643809 after";
            output = "before [imei] after";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@imei", (7, 13)),
            ];
        );
        assert_text_rule!(
            rule = "@imei:replace";
            input = "before 356938035643809 after";
            output = "before [imei] after";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@imei:replace", (7, 13)),
            ];
        );
        assert_text_rule!(
            rule = "@imei:hash";
            input = "before 356938035643809 after";
            output = "before 3888108AA99417402969D0B47A2CA4ECD2A1AAD3 after";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@imei:hash", (7, 47)),
            ];
        );
    }

    #[test]
    fn test_mac() {
        assert_text_rule!(
            rule = "@mac";
            input = "ether 4a:00:04:10:9b:50";
            output = "ether 4a:00:04:**:**:**";
            remarks = vec![
                Remark::with_range(RemarkType::Masked, "@mac", (6, 23)),
            ];
        );
        assert_text_rule!(
            rule = "@mac:mask";
            input = "ether 4a:00:04:10:9b:50";
            output = "ether 4a:00:04:**:**:**";
            remarks = vec![
                Remark::with_range(RemarkType::Masked, "@mac:mask", (6, 23)),
            ];
        );
        assert_text_rule!(
            rule = "@mac:replace";
            input = "ether 4a:00:04:10:9b:50";
            output = "ether [mac]";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@mac:replace", (6, 11)),
            ];
        );
        assert_text_rule!(
            rule = "@mac:hash";
            input = "ether 4a:00:04:10:9b:50";
            output = "ether 6220F3EE59BF56B32C98323D7DE43286AAF1F8F1";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@mac:hash", (6, 46)),
            ];
        );
    }

    #[test]
    fn test_email() {
        assert_text_rule!(
            rule = "@email";
            input = "John Appleseed <john@appleseed.com>";
            output = "John Appleseed <[email]>";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@email", (16, 23)),
            ];
        );
        assert_text_rule!(
            rule = "@email:replace";
            input = "John Appleseed <john@appleseed.com>";
            output = "John Appleseed <[email]>";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@email:replace", (16, 23)),
            ];
        );
        assert_text_rule!(
            rule = "@email:mask";
            input = "John Appleseed <john@appleseed.com>";
            output = "John Appleseed <****@*********.***>";
            remarks = vec![
                Remark::with_range(RemarkType::Masked, "@email:mask", (16, 34)),
            ];
        );
        assert_text_rule!(
            rule = "@email:hash";
            input = "John Appleseed <john@appleseed.com>";
            output = "John Appleseed <33835528AC0FFF1B46D167C35FEAAA6F08FD3F46>";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@email:hash", (16, 56)),
            ];
        );
    }

    #[test]
    fn test_creditcard() {
        assert_text_rule!(
            rule = "@creditcard";
            input = "John Appleseed 1234-1234-1234-1234!";
            output = "John Appleseed ****-****-****-1234!";
            remarks = vec![
                Remark::with_range(RemarkType::Masked, "@creditcard", (15, 34)),
            ];
        );
        assert_text_rule!(
            rule = "@creditcard:mask";
            input = "John Appleseed 1234-1234-1234-1234!";
            output = "John Appleseed ****-****-****-1234!";
            remarks = vec![
                Remark::with_range(RemarkType::Masked, "@creditcard:mask", (15, 34)),
            ];
        );
        assert_text_rule!(
            rule = "@creditcard:replace";
            input = "John Appleseed 1234-1234-1234-1234!";
            output = "John Appleseed [creditcard]!";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@creditcard:replace", (15, 27)),
            ];
        );
        assert_text_rule!(
            rule = "@creditcard:hash";
            input = "John Appleseed 1234-1234-1234-1234!";
            output = "John Appleseed 97227DBC2C4F028628CE96E0A3777F97C07BBC84!";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@creditcard:hash", (15, 55)),
            ];
        );
    }

    #[test]
    fn test_pemkey() {
        assert_text_rule!(
            rule = "@pemkey";
            input = "This is a comment I left on my key

            -----BEGIN EC PRIVATE KEY-----
MIHbAgEBBEFbLvIaAaez3q0u6BQYMHZ28B7iSdMPPaODUMGkdorl3ShgTbYmzqGL
fojr3jkJIxFS07AoLQpsawOFZipxSQHieaAHBgUrgQQAI6GBiQOBhgAEAdaBHsBo
KBuPVQL2F6z57Xb034TRlpaarne/XGWaCsohtl3YYcFll3A7rV+wHudcKFSvrgjp
soH5cUhpnhOL9ujuAM7Ldk1G+11Mf7EZ5sjrLe81fSB8S16D2vjtxkf/+mmwwhlM
HdmUCGvfKiF2CodxyLon1XkK8pX+Ap86MbJhluqK
-----END EC PRIVATE KEY-----";
            output = "This is a comment I left on my key

            -----BEGIN EC PRIVATE KEY-----
[pemkey]
-----END EC PRIVATE KEY-----";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@pemkey", (79, 87)),
            ];
        );
        assert_text_rule!(
            rule = "@pemkey:hash";
            input = "This is a comment I left on my key

            -----BEGIN EC PRIVATE KEY-----
MIHbAgEBBEFbLvIaAaez3q0u6BQYMHZ28B7iSdMPPaODUMGkdorl3ShgTbYmzqGL
fojr3jkJIxFS07AoLQpsawOFZipxSQHieaAHBgUrgQQAI6GBiQOBhgAEAdaBHsBo
KBuPVQL2F6z57Xb034TRlpaarne/XGWaCsohtl3YYcFll3A7rV+wHudcKFSvrgjp
soH5cUhpnhOL9ujuAM7Ldk1G+11Mf7EZ5sjrLe81fSB8S16D2vjtxkf/+mmwwhlM
HdmUCGvfKiF2CodxyLon1XkK8pX+Ap86MbJhluqK
-----END EC PRIVATE KEY-----";
            output = "This is a comment I left on my key

            -----BEGIN EC PRIVATE KEY-----
291134FEC77C62B11E2DC6D89910BB43157294BC
-----END EC PRIVATE KEY-----";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@pemkey:hash", (79, 119)),
            ];
        );
    }

    #[test]
    fn test_urlauth() {
        assert_text_rule!(
            rule = "@urlauth";
            input = "https://username:password@example.com/";
            output = "https://[auth]@example.com/";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@urlauth", (8, 14)),
            ];
        );
        assert_text_rule!(
            rule = "@urlauth:replace";
            input = "https://username:password@example.com/";
            output = "https://[auth]@example.com/";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@urlauth:replace", (8, 14)),
            ];
        );
        assert_text_rule!(
            rule = "@urlauth:hash";
            input = "https://username:password@example.com/";
            output = "https://2F82AB8A5E3FAD655B5F81E3BEA30D2A14FEF2AC@example.com/";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@urlauth:hash", (8, 48)),
            ];
        );
    }

    #[test]
    fn test_userpath() {
        assert_text_rule!(
            rule = "@userpath";
            input = "C:\\Users\\mitsuhiko\\Desktop";
            output = "C:\\Users\\[user]\\Desktop";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@userpath", (9, 15)),
            ];
        );
        assert_text_rule!(
            rule = "@userpath:replace";
            input = "File in /Users/mitsuhiko/Development/sentry-stripping";
            output = "File in /Users/[user]/Development/sentry-stripping";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@userpath:replace", (15, 21)),
            ];
        );
        assert_text_rule!(
            rule = "@userpath:replace";
            input = "C:\\Windows\\Profiles\\Armin\\Temp";
            output = "C:\\Windows\\Profiles\\[user]\\Temp";
            remarks = vec![
                Remark::with_range(RemarkType::Substituted, "@userpath:replace", (20, 26)),
            ];
        );
        assert_text_rule!(
            rule = "@userpath:hash";
            input = "File in /Users/mitsuhiko/Development/sentry-stripping";
            output = "File in /Users/A8791A1A8D11583E0200CC1B9AB971B4D78B8A69/Development/sentry-stripping";
            remarks = vec![
                Remark::with_range(RemarkType::Pseudonymized, "@userpath:hash", (15, 55)),
            ];
        );
    }
}
