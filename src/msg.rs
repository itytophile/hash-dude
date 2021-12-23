use alphabet::get_number_from_word;

#[derive(Debug, Clone)]
pub enum Message {
    Search(String, std::ops::Range<usize>),
    Stop,
    Exit,
}
#[derive(Debug)]
pub enum ConversionError {
    UnknownRequest,
    WordProblem(String),
}

impl TryFrom<&str> for Message {
    type Error = ConversionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let split: Vec<&str> = value.split(' ').collect();
        match split.as_slice() {
            &["search", hash, begin, end] => {
                match (get_number_from_word(begin), get_number_from_word(end)) {
                    (Ok(begin_num), Ok(end_num)) => {
                        Ok(Message::Search(hash.to_owned(), begin_num..end_num))
                    }
                    (Ok(_), Err(err)) => Err(ConversionError::WordProblem(format!(
                        "Problem with end word: {}",
                        err
                    ))),
                    (Err(err), Ok(_)) => Err(ConversionError::WordProblem(format!(
                        "Problem with begin word: {}",
                        err
                    ))),
                    (Err(err0), Err(err1)) => Err(ConversionError::WordProblem(format!(
                        "Problem with both words: {};{}",
                        err0, err1
                    ))),
                }
            }
            ["stop"] => Ok(Message::Stop),
            ["exit"] => Ok(Message::Exit),
            _ => Err(ConversionError::UnknownRequest),
        }
    }
}
