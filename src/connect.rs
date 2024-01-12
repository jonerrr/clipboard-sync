use aes_gcm::{
    aead::{generic_array::GenericArray, Aead, AeadCore, OsRng},
    Aes256Gcm,
};
use arboard::Clipboard;
use tokio_tungstenite::tungstenite::protocol::Message;

pub async fn read_clipboard(
    tx: futures_channel::mpsc::UnboundedSender<Message>,
    cipher: Option<Aes256Gcm>,
) {
    let mut clipboard = Clipboard::new().unwrap();
    let mut last_clipboard: Vec<u8> = Vec::new();

    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        let data = match clipboard.get_text() {
            Ok(data) => {
                let mut data_vec: Vec<u8> = Vec::new();
                // extend the vector first with the data type (text = 0 and image = 1)
                data_vec.extend_from_slice(&(0u8).to_be_bytes());
                data_vec.extend_from_slice(data.as_bytes());
                data_vec
            }
            Err(_) => {
                if let Ok(data) = clipboard.get_image() {
                    let mut data_vec = Vec::new();
                    data_vec.extend_from_slice(&(1u8).to_be_bytes());
                    // include image data dimensions
                    data_vec.extend_from_slice(&data.width.to_be_bytes());
                    data_vec.extend_from_slice(&data.height.to_be_bytes());
                    data_vec.extend_from_slice(&data.bytes);
                    data_vec
                } else {
                    continue;
                }
            }
        };

        if last_clipboard != data {
            last_clipboard = data.clone();

            if let Some(cipher) = &cipher {
                let nonce: GenericArray<u8, _> = Aes256Gcm::generate_nonce(&mut OsRng);
                let ciphertext = cipher.encrypt(&nonce, data.as_ref()).unwrap();
                // send nonce and ciphertext
                let mut data_vec = Vec::new();
                data_vec.extend_from_slice(&nonce);
                data_vec.extend_from_slice(&ciphertext);
                tx.unbounded_send(Message::binary(data_vec)).unwrap();

                continue;
            }

            tx.unbounded_send(Message::binary(data)).unwrap();
        }
    }
}
