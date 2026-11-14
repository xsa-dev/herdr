//! Pure-Rust XTGETTCAP capability table matching Ghostty's bundled terminfo capabilities.

#[derive(Clone, Copy)]
enum CapVal {
    Bool,
    Str(&'static [u8]),
}

/// Looks up a raw capability name (decoded from XTGETTCAP hex) and writes its response
/// `\x1bP1+r<UPPER_HEX_KEY>[=<UPPER_HEX_VAL>]\x1b\\` if supported.
pub(crate) fn lookup_xtgettcap(name: &[u8], terminfo_name: &str) -> Option<Vec<u8>> {
    if name == b"TN" {
        if terminfo_name.is_empty() || terminfo_name.len() > 128 {
            return None;
        }
        return Some(encode_xtgettcap_reply(name, Some(terminfo_name.as_bytes())));
    }

    let val = match name {
        // Synthetic XTGETTCAP keys
        b"Co" | b"colors" => CapVal::Str(b"256"),
        b"RGB" => CapVal::Str(b"8"),
        b"cols" => CapVal::Str(b"80"),
        b"it" => CapVal::Str(b"8"),
        b"lines" => CapVal::Str(b"24"),
        b"pairs" => CapVal::Str(b"32767"),

        // Boolean capabilities
        b"am" | b"bce" | b"ccc" | b"hs" | b"km" | b"mc5i" | b"mir" | b"msgr" | b"npc"
        | b"xenl" | b"AX" | b"Tc" | b"Su" | b"XT" | b"fullkbd" => CapVal::Bool,

        // Parameterized string capabilities (preserved in terminfo source form per Ghostty/Kitty)
        b"Smulx" => CapVal::Str(b"\\E[4:%p1%dm"),
        b"Setulc" => CapVal::Str(
            b"\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
        ),
        b"Ss" => CapVal::Str(b"\\E[%p1%d q"),
        b"Ms" => CapVal::Str(b"\\E]52;%p1%s;%p2%s\\007"),
        b"Sync" => CapVal::Str(b"\\E[?2026%?%p1%{1}%-%tl%eh%;"),
        b"XM" => CapVal::Str(b"\\E[?1006;1000%?%p1%{1}%=%th%el%;"),
        b"xm" => CapVal::Str(b"\\E[<%i%p3%d;%p1%d;%p2%d;%?%p4%tM%em%;"),
        b"Cmg" => CapVal::Str(b"\\E[%i%p1%d;%p2%ds"),
        b"csr" => CapVal::Str(b"\\E[%i%p1%d;%p2%dr"),
        b"cub" => CapVal::Str(b"\\E[%p1%dD"),
        b"cud" => CapVal::Str(b"\\E[%p1%dB"),
        b"cuf" => CapVal::Str(b"\\E[%p1%dC"),
        b"cup" => CapVal::Str(b"\\E[%i%p1%d;%p2%dH"),
        b"cuu" => CapVal::Str(b"\\E[%p1%dA"),
        b"dch" => CapVal::Str(b"\\E[%p1%dP"),
        b"dl" => CapVal::Str(b"\\E[%p1%dM"),
        b"ech" => CapVal::Str(b"\\E[%p1%dX"),
        b"hpa" => CapVal::Str(b"\\E[%i%p1%dG"),
        b"ich" => CapVal::Str(b"\\E[%p1%d@"),
        b"il" => CapVal::Str(b"\\E[%p1%dL"),
        b"indn" => CapVal::Str(b"\\E[%p1%dS"),
        b"initc" => CapVal::Str(
            b"\\E]4;%p1%d;rgb\\:%p2%{255}%*%{1000}%/%2.2X/%p3%{255}%*%{1000}%/%2.2X/%p4%{255}%*%{1000}%/%2.2X\\E\\\\",
        ),
        b"rep" => CapVal::Str(b"%p1%c\\E[%p2%{1}%-%db"),
        b"rin" => CapVal::Str(b"\\E[%p1%dT"),
        b"setab" => CapVal::Str(
            b"\\E[%?%p1%{8}%<%t4%p1%d%e%p1%{16}%<%t10%p1%{8}%-%d%e48;5;%p1%d%;m",
        ),
        b"setaf" => CapVal::Str(
            b"\\E[%?%p1%{8}%<%t3%p1%d%e%p1%{16}%<%t9%p1%{8}%-%d%e38;5;%p1%d%;m",
        ),
        b"setrgbb" => CapVal::Str(b"\\E[48:2:%p1%d:%p2%d:%p3%dm"),
        b"setrgbf" => CapVal::Str(b"\\E[38:2:%p1%d:%p2%d:%p3%dm"),
        b"sgr" => CapVal::Str(
            b"%?%p9%t\\E(0%e\\E(B%;\\E[0%?%p6%t;1%;%?%p5%t;2%;%?%p2%t;4%;%?%p1%p3%|%t;7%;%?%p4%t;5%;%?%p7%t;8%;m",
        ),
        b"u6" => CapVal::Str(b"\\E[%i%d;%dR"),
        b"u8" => CapVal::Str(b"\\E[?%[;0123456789]c"),
        b"vpa" => CapVal::Str(b"\\E[%i%p1%dd"),

        // Unparameterized string capabilities (unescaped \E -> 0x1b and ^X -> control byte)
        b"acsc" => CapVal::Str(
            b"++\\,\\,--..00``aaffgghhiijjkkllmmnnooppqqrrssttuuvvwwxxyyzz{{||}}~~",
        ),
        b"Smol" => CapVal::Str(b"\x1b[53m"),
        b"Rmol" => CapVal::Str(b"\x1b[55m"),
        b"Se" => CapVal::Str(b"\x1b[0 q"),
        b"BD" => CapVal::Str(b"\x1b[?2004l"),
        b"BE" => CapVal::Str(b"\x1b[?2004h"),
        b"PS" => CapVal::Str(b"\x1b[200~"),
        b"PE" => CapVal::Str(b"\x1b[201~"),
        b"RV" => CapVal::Str(b"\x1b[>c"),
        b"rv" => CapVal::Str(b"\x1b\\[[0-9]+;[0-9]+;[0-9]+c"),
        b"XR" => CapVal::Str(b"\x1b[>0q"),
        b"xr" => CapVal::Str(b"\x1bP>\\|[ -~]+a\x1b\\"),
        b"Enmg" => CapVal::Str(b"\x1b[?69h"),
        b"Dsmg" => CapVal::Str(b"\x1b[?69l"),
        b"Clmg" => CapVal::Str(b"\x1b[s"),
        b"clear" => CapVal::Str(b"\x1b[H\x1b[2J"),
        b"E3" => CapVal::Str(b"\x1b[3J"),
        b"fe" => CapVal::Str(b"\x1b[?1004h"),
        b"fd" => CapVal::Str(b"\x1b[?1004l"),
        b"kxIN" => CapVal::Str(b"\x1b[I"),
        b"kxOUT" => CapVal::Str(b"\x1b[O"),
        b"bel" | b"fsl" => CapVal::Str(b"\x07"),
        b"blink" => CapVal::Str(b"\x1b[5m"),
        b"bold" => CapVal::Str(b"\x1b[1m"),
        b"cbt" | b"kcbt" => CapVal::Str(b"\x1b[Z"),
        b"civis" => CapVal::Str(b"\x1b[?25l"),
        b"cnorm" => CapVal::Str(b"\x1b[?12l\x1b[?25h"),
        b"cr" => CapVal::Str(b"\\r"),
        b"cub1" => CapVal::Str(b"\x08"),
        b"cud1" => CapVal::Str(b"\x0a"),
        b"cuf1" => CapVal::Str(b"\x1b[C"),
        b"cuu1" => CapVal::Str(b"\x1b[A"),
        b"cvvis" => CapVal::Str(b"\x1b[?12;25h"),
        b"dch1" => CapVal::Str(b"\x1b[P"),
        b"dim" => CapVal::Str(b"\x1b[2m"),
        b"dl1" => CapVal::Str(b"\x1b[M"),
        b"dsl" => CapVal::Str(b"\x1b]2;\\007"),
        b"ed" => CapVal::Str(b"\x1b[J"),
        b"el" => CapVal::Str(b"\x1b[K"),
        b"el1" => CapVal::Str(b"\x1b[1K"),
        b"flash" => CapVal::Str(b"\x1b[?5h$<100/>\x1b[?5l"),
        b"home" => CapVal::Str(b"\x1b[H"),
        b"ht" => CapVal::Str(b"\x09"),
        b"hts" => CapVal::Str(b"\x1bH"),
        b"ich1" => CapVal::Str(b"\x1b[@"),
        b"il1" => CapVal::Str(b"\x1b[L"),
        b"ind" => CapVal::Str(b"\\n"),
        b"invis" => CapVal::Str(b"\x1b[8m"),
        b"oc" => CapVal::Str(b"\x1b]104\\007"),
        b"op" => CapVal::Str(b"\x1b[39;49m"),
        b"rc" => CapVal::Str(b"\x1b8"),
        b"rev" | b"smso" => CapVal::Str(b"\x1b[7m"),
        b"ri" => CapVal::Str(b"\x1bM"),
        b"ritm" => CapVal::Str(b"\x1b[23m"),
        b"rmacs" => CapVal::Str(b"\x1b(B"),
        b"rmam" => CapVal::Str(b"\x1b[?7l"),
        b"rmcup" => CapVal::Str(b"\x1b[?1049l"),
        b"rmir" => CapVal::Str(b"\x1b[4l"),
        b"rmkx" => CapVal::Str(b"\x1b[?1l\x1b>"),
        b"rmso" => CapVal::Str(b"\x1b[27m"),
        b"rmul" => CapVal::Str(b"\x1b[24m"),
        b"rmxx" => CapVal::Str(b"\x1b[29m"),
        b"sgr0" => CapVal::Str(b"\x1b(B\x1b[m"),
        b"sitm" => CapVal::Str(b"\x1b[3m"),
        b"smacs" => CapVal::Str(b"\x1b(0"),
        b"smam" => CapVal::Str(b"\x1b[?7h"),
        b"smcup" => CapVal::Str(b"\x1b[?1049h"),
        b"smir" => CapVal::Str(b"\x1b[4h"),
        b"smkx" => CapVal::Str(b"\x1b[?1h\x1b="),
        b"smul" => CapVal::Str(b"\x1b[4m"),
        b"smxx" => CapVal::Str(b"\x1b[9m"),
        b"tbc" => CapVal::Str(b"\x1b[3g"),
        b"tsl" => CapVal::Str(b"\x1b]2;"),
        b"u7" => CapVal::Str(b"\x1b[6n"),
        b"u9" => CapVal::Str(b"\x1b[c"),
        b"kbs" => CapVal::Str(b"\x7f"),
        b"kcub1" => CapVal::Str(b"\x1bOD"),
        b"kcud1" => CapVal::Str(b"\x1bOB"),
        b"kcuf1" => CapVal::Str(b"\x1bOC"),
        b"kcuu1" => CapVal::Str(b"\x1bOA"),
        b"kdch1" => CapVal::Str(b"\x1b[3~"),
        b"kend" => CapVal::Str(b"\x1bOF"),
        b"kent" => CapVal::Str(b"\x1bOM"),
        b"kf1" => CapVal::Str(b"\x1bOP"),
        b"kf2" => CapVal::Str(b"\x1bOQ"),
        b"kf3" => CapVal::Str(b"\x1bOR"),
        b"kf4" => CapVal::Str(b"\x1bOS"),
        b"kf5" => CapVal::Str(b"\x1b[15~"),
        b"kf6" => CapVal::Str(b"\x1b[17~"),
        b"kf7" => CapVal::Str(b"\x1b[18~"),
        b"kf8" => CapVal::Str(b"\x1b[19~"),
        b"kf9" => CapVal::Str(b"\x1b[20~"),
        b"kf10" => CapVal::Str(b"\x1b[21~"),
        b"kf11" => CapVal::Str(b"\x1b[23~"),
        b"kf12" => CapVal::Str(b"\x1b[24~"),
        b"khome" => CapVal::Str(b"\x1bOH"),
        b"kich1" => CapVal::Str(b"\x1b[2~"),
        b"kind" | b"kDN" => CapVal::Str(b"\x1b[1;2B"),
        b"kmous" => CapVal::Str(b"\x1b[<"),
        b"knp" => CapVal::Str(b"\x1b[6~"),
        b"kpp" => CapVal::Str(b"\x1b[5~"),
        b"kri" | b"kUP" => CapVal::Str(b"\x1b[1;2A"),
        b"kLFT" => CapVal::Str(b"\x1b[1;2D"),
        b"kRIT" => CapVal::Str(b"\x1b[1;2C"),
        b"kDC" => CapVal::Str(b"\x1b[3;2~"),
        b"kIC" => CapVal::Str(b"\x1b[2;2~"),
        b"kHOM" => CapVal::Str(b"\x1b[1;2H"),
        b"kEND" => CapVal::Str(b"\x1b[1;2F"),
        b"kNXT" => CapVal::Str(b"\x1b[6;2~"),
        b"kPRV" => CapVal::Str(b"\x1b[5;2~"),
        b"rs1" => CapVal::Str(b"\x1b]\x1b\\\x1bc"),
        b"sc" => CapVal::Str(b"\x1b7"),
        _ => return None,
    };

    match val {
        CapVal::Bool => Some(encode_xtgettcap_reply(name, None)),
        CapVal::Str(bytes) => Some(encode_xtgettcap_reply(name, Some(bytes))),
    }
}

pub(crate) fn decode_hex_bytes(hex: &[u8]) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn encode_xtgettcap_reply(key: &[u8], value: Option<&[u8]>) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let extra = value.map_or(0, |v| 1 + v.len() * 2);
    let mut out = Vec::with_capacity(5 + key.len() * 2 + extra + 2);
    out.extend_from_slice(b"\x1bP1+r");
    for &b in key {
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0F) as usize]);
    }
    if let Some(val) = value {
        out.push(b'=');
        for &b in val {
            out.push(HEX[(b >> 4) as usize]);
            out.push(HEX[(b & 0x0F) as usize]);
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}
