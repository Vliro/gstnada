use crate::gcctx::imp::parse_twcc;

static FIRST: u8 = 0b10001111;
static DATA: &[u8] = &[FIRST, 205, 0, 0, 0,0,0,0,0,0,0,0, 0, 32, 0, /* Packet status count */10, 0, 0, 32,  /* Feedback packet count */1,
    0b00100000,
    0b00000001,
    0b11111111
];
#[test]
pub fn test_twcc_packet() {
    let (data, count) = parse_twcc(DATA);
    println!("{data:?}")
}