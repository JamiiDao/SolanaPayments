use solana_client::nonblocking::rpc_client::RpcClient;
use solana_payments::SolanaPayUrl;
use spl_token_2022::{extension::StateWithExtensions, state::Mint};

#[tokio::main]
async fn main() {
    let lookup_fn = |public_key: [u8; 32]| async move {
        let client = RpcClient::new("https://api.mainnet-beta.solana.com".into());

        let public_key = solana_program::pubkey::Pubkey::new_from_array(public_key);
        let account = client.get_account(&public_key).await.unwrap();

        let mint = StateWithExtensions::<Mint>::unpack(&account.data).unwrap();
        mint.base.decimals
    };

    let url  = "solana:mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN?amount=0.01&spl-token=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let url_decoded = SolanaPayUrl::new().parse(url, lookup_fn).await.unwrap();

    dbg!(url_decoded);

    // Encoding data to a URL
    let solana_pay_url = SolanaPayUrl::default()
        .add_recipient("mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN")
        .unwrap_or_default()
        .add_label("Jamii Dao")
        .unwrap_or_default()
        .add_message("Thanks for buying coffee and keeping the lights on.")
        .unwrap_or_default()
        .to_url();

    println!("{solana_pay_url}")
}
