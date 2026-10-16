//! ICU's three-letter to two-letter code tables, which ICU applies whenever it
//! parses a locale name (`ulocimp_getLanguage` / `ulocimp_getCountry`), so
//! `mongod`'s `Did you mean` for `eng` is `en` and for `en_USA` is `en_US`.
//!
//! Read from ICU 78's `uloc_getISO3Language` / `uloc_getISO3Country` (macOS
//! libicucore) as `three:two` pairs; `mongod` links ICU 57.1. Every language
//! pair whose two-letter code `mongod` accepts as a locale, and the four regions
//! it accepts (AT, US, AF, CA), was checked against `mongod` 8.2.11 on
//! 2026-10-16. No other pair can change a reply: a code that maps to a locale
//! `mongod` does not accept is dropped from the suggestion either way.

pub(super) const LANGUAGES_3: &str = "\
    aar:aa abk:ab afr:af aka:ak amh:am ara:ar arg:an asm:as ava:av ave:ae aym:ay aze:az \
    bak:ba bam:bm bel:be ben:bn bis:bi bod:bo bos:bs bre:br bul:bg cat:ca ces:cs cha:ch \
    che:ce chu:cu chv:cv cor:kw cos:co cre:cr cym:cy dan:da deu:de div:dv dzo:dz ell:el \
    eng:en epo:eo est:et eus:eu ewe:ee fao:fo fas:fa fij:fj fin:fi fra:fr fry:fy ful:ff \
    gla:gd gle:ga glg:gl glv:gv grn:gn guj:gu hat:ht hau:ha heb:he her:hz hin:hi hmo:ho \
    hrv:hr hun:hu hye:hy ibo:ig ido:io iii:ii iku:iu ile:ie ina:ia ind:id ipk:ik isl:is \
    ita:it jav:jv jpn:ja kal:kl kan:kn kas:ks kat:ka kau:kr kaz:kk khm:km kik:ki kin:rw \
    kir:ky kom:kv kon:kg kor:ko kua:kj kur:ku lao:lo lat:la lav:lv lim:li lin:ln lit:lt \
    ltz:lb lub:lu lug:lg mah:mh mal:ml mar:mr mkd:mk mlg:mg mlt:mt mon:mn mri:mi msa:ms \
    mya:my nau:na nav:nv nbl:nr nde:nd ndo:ng nep:ne nld:nl nno:nn nob:nb nor:no nya:ny \
    oci:oc oji:oj ori:or orm:om oss:os pan:pa pli:pi pol:pl por:pt pus:ps que:qu roh:rm \
    ron:ro run:rn rus:ru sag:sg san:sa sin:si slk:sk slv:sl sme:se smo:sm sna:sn snd:sd \
    som:so sot:st spa:es sqi:sq srd:sc srp:sr ssw:ss sun:su swa:sw swe:sv tah:ty tam:ta \
    tat:tt tel:te tgk:tg tha:th tir:ti ton:to tsn:tn tso:ts tuk:tk tur:tr twi:tw uig:ug \
    ukr:uk urd:ur uzb:uz ven:ve vie:vi vol:vo wln:wa wol:wo xho:xh yid:yi yor:yo zha:za \
    zho:zh zul:zu";

pub(super) const COUNTRIES_3: &str = "\
    ABW:AW AFG:AF AGO:AO AIA:AI ALA:AX ALB:AL AND:AD ARE:AE ARG:AR ARM:AM ASC:AC ASM:AS \
    ATA:AQ ATF:TF ATG:AG AUS:AU AUT:AT AZE:AZ BDI:BI BEL:BE BEN:BJ BES:BQ BFA:BF BGD:BD \
    BGR:BG BHR:BH BHS:BS BIH:BA BLM:BL BLR:BY BLZ:BZ BMU:BM BOL:BO BRA:BR BRB:BB BRN:BN \
    BTN:BT BVT:BV BWA:BW CAF:CF CAN:CA CCK:CC CHE:CH CHL:CL CHN:CN CIV:CI CMR:CM COD:CD \
    COG:CG COK:CK COL:CO COM:KM CPT:CP CPV:CV CRI:CR CRQ:CQ CUB:CU CUW:CW CXR:CX CYM:KY \
    CYP:CY CZE:CZ DEU:DE DGA:DG DJI:DJ DMA:DM DNK:DK DOM:DO DZA:DZ ECU:EC EGY:EG ERI:ER \
    ESH:EH ESP:ES EST:EE ETH:ET FIN:FI FJI:FJ FLK:FK FRA:FR FRO:FO FSM:FM GAB:GA GBR:GB \
    GEO:GE GGY:GG GHA:GH GIB:GI GIN:GN GLP:GP GMB:GM GNB:GW GNQ:GQ GRC:GR GRD:GD GRL:GL \
    GTM:GT GUF:GF GUM:GU GUY:GY HKG:HK HMD:HM HND:HN HRV:HR HTI:HT HUN:HU IDN:ID IMN:IM \
    IND:IN IOT:IO IRL:IE IRN:IR IRQ:IQ ISL:IS ISR:IL ITA:IT JAM:JM JEY:JE JOR:JO JPN:JP \
    KAZ:KZ KEN:KE KGZ:KG KHM:KH KIR:KI KNA:KN KOR:KR KWT:KW LAO:LA LBN:LB LBR:LR LBY:LY \
    LCA:LC LIE:LI LKA:LK LSO:LS LTU:LT LUX:LU LVA:LV MAC:MO MAF:MF MAR:MA MCO:MC MDA:MD \
    MDG:MG MDV:MV MEX:MX MHL:MH MKD:MK MLI:ML MLT:MT MMR:MM MNE:ME MNG:MN MNP:MP MOZ:MZ \
    MRT:MR MSR:MS MTQ:MQ MUS:MU MWI:MW MYS:MY MYT:YT NAM:NA NCL:NC NER:NE NFK:NF NGA:NG \
    NIC:NI NIU:NU NLD:NL NOR:NO NPL:NP NRU:NR NZL:NZ OMN:OM PAK:PK PAN:PA PCN:PN PER:PE \
    PHL:PH PLW:PW PNG:PG POL:PL PRI:PR PRK:KP PRT:PT PRY:PY PSE:PS PYF:PF QAT:QA REU:RE \
    ROU:RO RUS:RU RWA:RW SAU:SA SDN:SD SEN:SN SGP:SG SGS:GS SHN:SH SJM:SJ SLB:SB SLE:SL \
    SLV:SV SMR:SM SOM:SO SPM:PM SRB:RS SSD:SS STP:ST SUR:SR SVK:SK SVN:SI SWE:SE SWZ:SZ \
    SXM:SX SYC:SC SYR:SY TAA:TA TCA:TC TCD:TD TGO:TG THA:TH TJK:TJ TKL:TK TKM:TM TLS:TL \
    TON:TO TTO:TT TUN:TN TUR:TR TUV:TV TWN:TW TZA:TZ UGA:UG UKR:UA UMI:UM URY:UY USA:US \
    UZB:UZ VAT:VA VCT:VC VEN:VE VGB:VG VIR:VI VNM:VN VUT:VU WLF:WF WSM:WS XEA:EA XIC:IC \
    XKK:XK YEM:YE ZAF:ZA ZMB:ZM ZWE:ZW";

/// The two-letter code ICU reads a three-letter one as, if it has one.
pub(super) fn two_letter(table: &'static str, three: &str) -> Option<&'static str> {
    table
        .split(' ')
        .filter_map(|pair| pair.split_once(':'))
        .find(|(t, _)| *t == three)
        .map(|(_, two)| two)
}
