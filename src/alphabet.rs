// algo trouvé par chance
pub fn get_word_from_number(mut num: usize) -> String {
    let mut word = String::with_capacity(10);
    loop {
        word.insert(0, ALPHABET[num % BASE]);
        num /= BASE;
        if num == 0 {
            break word;
        }
        num -= 1; // la petite douille de la chance
    }
}

// optimisation pour Mike
pub fn get_bytes_from_number<const N: usize>(mut num: usize, buffer: &mut [u8; N]) -> &[u8] {
    for i in (0..N).rev() {
        buffer[i] = ALPHABET[num % BASE] as u8;
        num /= BASE;
        if num == 0 {
            return &buffer[i..];
        }
        num -= 1;
    }
    &[]
}

pub fn increment_word<const N: usize>(buffer: &mut [u8; N]) -> Option<&'_ [u8]> {
    for i in (0..N).rev() {
        match buffer[i] {
            b'9' => buffer[i] = b'a',
            0 => {
                buffer[i] = b'a';
                return Some(&buffer[i..N]);
            }
            byte => {
                match byte {
                    b'z' => {
                        buffer[i] = b'A';
                    }
                    b'Z' => {
                        buffer[i] = b'0';
                    }
                    _ => {
                        buffer[i] += 1;
                    }
                };
                break;
            }
        }
    }
    None
}

// algo trouvé aussi par chance
pub fn get_number_from_word(word: &str) -> Result<usize, &'static str> {
    let mut num = 0;
    for (index, c) in word.chars().rev().enumerate() {
        let letter_index = match ALPHABET.iter().position(|&a| a == c) {
            Some(index) => index,
            None => return Err("Unsupported letter in word"),
        };

        if usize::MAX / (letter_index + 1) < 62_u64.pow(index as u32) as usize {
            return Err(
                "The word value is overflowing the usize capacity, use a smaller word please",
            );
        }

        let addition = (letter_index + 1) * 62_u64.pow(index as u32) as usize;

        // Pour éviter l'overflow
        if num > usize::MAX - addition {
            return Err(
                "The word value is overflowing the usize capacity, use a smaller word please",
            );
        }

        num += addition;
    }
    Ok(num - 1)
}

const BASE: usize = 62;

const ALPHABET: [char; BASE] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L',
    'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '0', '1', '2', '3', '4',
    '5', '6', '7', '8', '9',
];
