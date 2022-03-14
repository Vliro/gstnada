use bitreader::BitReader;
use crate::nadatx::imp::parse_twcc;

static FIRST: u8 = 0b10001111;
static DATA: &[u8] = &[FIRST, 205, 0, 0, 0,0,0,0,0,0,0,0, 0, 32, 0, /* Packet status count */10, 0, 0, 32,  /* Feedback packet count */1,
    0b00100000,
    0b00000001,
    0b11111111
];

static D: &[u8] = &[128, 200, 0, 6, 0, 0, 0, 1, 229, 211, 1, 43, 197, 190, 110, 101, 99, 198, 60, 115, 0, 0, 0, 142, 0, 0, 52, 218, 129, 202, 0, 12, 0, 0, 0, 1, 1, 28, 117, 115, 101, 114, 49, 52, 48, 53, 54, 57, 51, 51, 48, 48, 64, 104, 111, 115, 116, 45, 101, 54, 56, 52, 53, 53, 50, 51, 6, 9, 71, 83, 116, 114, 101, 97, 109, 101, 114, 0, 0, 0];
#[test]
pub fn test_twcc_packet() {
    //parse_twcc(D);
  //  println!("{data:?}")
}
#[test]
pub fn parse_test() {
    let b  = [49,40,147,0];
    let mut r = BitReader::new(&b);
    r.read_u8(8).unwrap();
    let x = r.read_u16(16).unwrap();// u16::from_be_bytes([b[3], b[2]]);
    let t = u16::from_be_bytes([b[1], b[2]]);;
    println!("{x}");
    println!("{t}");
}