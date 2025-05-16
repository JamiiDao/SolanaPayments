use std::{borrow::Cow, future::Future};

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

use crate::{Number, PublicKey, Reference, SolanaPayError, SolanaPayResult, Utils, SOLANA_SCHEME};

// TODO Create program derived addresses

/// Structure of a Solana Pay URL.
/// ***Credit:*** [Solana Pay Docs](https://docs.solanapay.com/spec)
///
/// ```sh
/// solana:<recipient>
///     ?amount=<amount>
///     &spl-token=<spl-token>
///     &reference=<reference>
///     &label=<label>
///     &message=<message>
/// ```
#[derive(Debug, Default, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SolanaPayUrl<'a> {
    /// A single recipient field is required as the pathname.
    /// The value must be the base58-encoded public key of a native SOL account.
    /// Associated token accounts must not be used.
    /// Instead, to request an SPL Token transfer, the spl-token field must be used to specify an SPL Token mint,
    /// from which the associated token address of the recipient must be derived.
    pub recipient: PublicKey,
    /// A single amount field is allowed as an optional query parameter.
    /// The value must be a non-negative integer or decimal number of "user" units.
    /// For SOL, that's SOL and not lamports. For tokens, use uiAmountString and not amount.
    /// 0 is a valid value. If the value is a decimal number less than 1,
    /// it must have a leading `0` before the `.`. Scientific notation is prohibited.
    /// If a value is not provided, the wallet must prompt the user for the amount.
    /// If the number of decimal places exceed what's supported for SOL (9) or the SPL Token (mint specific),
    /// the wallet must reject the URL as malformed.
    pub amount: Option<Number<'a>>,
    /// A single spl-token field is allowed as an optional query parameter.
    /// The value must be the base58-encoded public key of an SPL Token mint account.
    /// If the field is provided, the Associated Token Account convention must be used,
    /// and the wallet must include a `TokenProgram.Transfer` or `TokenProgram.TransferChecked` instruction
    /// as the last instruction of the transaction.
    /// If the field is not provided, the URL describes a native SOL transfer,
    /// and the wallet must include a SystemProgram.Transfer instruction as the last instruction of the transaction instead.
    /// The wallet must derive the ATA address from the recipient and spl-token fields.
    /// Transfers to auxiliary token accounts are not supported.
    pub spl_token: Option<PublicKey>,
    /// Multiple reference fields are allowed as optional query parameters.
    /// The values must be base58-encoded 32 byte arrays.
    /// These may or may not be public keys, on or off the curve, and may or may not correspond with accounts on Solana.
    /// If the values are provided, the wallet must include them in the order provided as read-only,
    /// non-signer keys to the `SystemProgram.Transfer` or `TokenProgram.Transfer/TokenProgram.TransferChecked`
    /// instruction in the payment transaction. The values may or may not be unique to the payment request,
    /// and may or may not correspond to an account on Solana. Because Solana validators index transactions
    /// by these account keys, reference values can be used as client IDs
    /// (IDs usable before knowing the eventual payment transaction).
    /// The `getSignaturesForAddress` RPC method can be used locate transactions this way.
    /// One or multiple references as defined by [Solana Pay Spec](https://docs.solanapay.com/spec#reference).
    pub references: Vec<Reference>,
    /// A single label field is allowed as an optional query parameter.
    /// The value must be a URL-encoded UTF-8 string that describes the source of the transfer request.
    /// For example, this might be the name of a brand, store, application, or person making the request.
    /// The wallet should URL-decode the value and display the decoded value to the user.
    pub label: Option<Cow<'a, str>>,
    /// A single message field is allowed as an optional query parameter.
    /// The value must be a URL-encoded UTF-8 string that describes the nature of the transfer request.
    /// For example, this might be the name of an item being purchased, an order ID, or a thank you note.
    /// The wallet should URL-decode the value and display the decoded value to the user.
    pub message: Option<Cow<'a, str>>,
    /// A single memo field is allowed as an optional query parameter.
    /// The value must be a URL-encoded UTF-8 string that must be included in an SPL Memo instruction in the payment transaction.
    /// The wallet must URL-decode the value and should display the decoded value to the user.
    /// The memo will be recorded by validators and should not include private or sensitive information.
    /// If the field is provided, the wallet must include a MemoProgram instruction as the second to last
    /// instruction of the transaction, immediately before the SOL or SPL Token transfer instruction,
    /// to avoid ambiguity with other instructions in the transaction.
    pub spl_memo: Option<Cow<'a, str>>,
}

impl<'a> SolanaPayUrl<'a> {
    /// Instantiate a new url
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a Solana Pay URL
    pub async fn parse<F: Fn([u8; 32]) -> Fut, Fut: Future<Output = u8> + Send + 'static>(
        mut self,
        solana_pay_url: &'a str,
        lookup_fn: F,
    ) -> SolanaPayResult<Self> {
        if !solana_pay_url.starts_with(SOLANA_SCHEME) {
            panic!("InvalidSolanaPayScheme");
        }

        let decoded = solana_pay_url.split(SOLANA_SCHEME).collect::<Vec<&str>>()[1];
        let first_split = if decoded.contains('?') {
            decoded.split('?').collect::<Vec<&str>>()
        } else {
            decoded.split('&').collect::<Vec<&str>>()
        };

        if let Some(base58_public_key) = first_split.first() {
            self.recipient = PublicKey::from_base58(base58_public_key)?;
        } else {
            panic!("SolanaPayUrlPartsEmpty");
        };

        if first_split.len() > 2 {
            panic!("TooManySolanaPayUrlParts");
        }

        let mut queries = Vec::<&str>::new();
        if let Some(options) = first_split.get(1) {
            options.split("&").for_each(|value| queries.push(value))
        }

        for query in queries {
            let split_query = query.split('=').collect::<Vec<&str>>();
            if split_query.len() != 2 {
                panic!("InvalidQuery");
            }

            let query_param: QueryParam = split_query[0].try_into()?;
            let value_to_parse = split_query[1];
            match query_param {
                QueryParam::Amount => {
                    if self.amount.is_some() {
                        return Err(SolanaPayError::AmountAlreadyExists);
                    }
                    self.amount.replace(Number::new(value_to_parse).parse()?);
                }

                QueryParam::SplToken => {
                    if self.spl_token.is_some() {
                        return Err(SolanaPayError::SplTokenAlreadyExists);
                    }

                    self.spl_token
                        .replace(PublicKey::from_base58(value_to_parse)?);

                    self.resolve_decimals(&lookup_fn).await?;

                    // If this is true then the amount is native SOL and therefore
                    // check the number of decimals don't exceed 9 decimal places

                    if self.amount.is_some()
                        && self.spl_token.is_none()
                        && self.amount.as_ref().unwrap().total_fractional_count
                            > crate::NATIVE_SOL_DECIMAL_COUNT as usize
                    {
                        return Err(SolanaPayError::NumberOfDecimalsExceeds9);
                    }
                }

                QueryParam::Reference => {
                    if self.references.len() > crate::MAX_ACCOUNTS_PER_TX {
                        return Err(SolanaPayError::TooManyReferences);
                    }

                    self.references
                        .push(Reference::from_base58(value_to_parse)?)
                }

                QueryParam::Label => {
                    if self.label.is_some() {
                        return Err(SolanaPayError::LabelAlreadyExists);
                    }

                    self.label.replace(Utils::url_decode(value_to_parse)?);
                }

                QueryParam::Message => {
                    if self.message.is_some() {
                        return Err(SolanaPayError::MessageAlreadyExists);
                    }

                    self.message.replace(Utils::url_decode(value_to_parse)?);
                }

                QueryParam::SplMemo => {
                    if self.spl_memo.is_some() {
                        return Err(SolanaPayError::MemoAlreadyExists);
                    }

                    self.spl_memo.replace(Utils::url_decode(value_to_parse)?);
                }
                QueryParam::Unsupported => return Err(SolanaPayError::InvalidQueryParam),
            };
        }

        Ok(self)
    }

    /// Add a Base58 encoded Ed25519 public key for the recipient
    pub fn add_recipient(mut self, base58_public_key: &str) -> SolanaPayResult<Self> {
        let recipient = PublicKey::from_base58(base58_public_key)?;

        if !recipient.is_on_ed25519_curve()? {
            return Err(SolanaPayError::ExpectedRecipientPublicKeyOnCurve);
        }

        self.recipient = recipient;

        Ok(self)
    }

    /// A single amount field is allowed as an optional query parameter.
    /// The value must be a non-negative integer or decimal number of "user" units. For SOL, that's SOL and not lamports.
    pub fn add_amount(mut self, amount: &'a str) -> SolanaPayResult<Self> {
        if self.amount.is_some() {
            return Err(SolanaPayError::AmountAlreadyExists);
        }

        let amount = Number::new(amount).parse()?;
        self.amount.replace(amount);

        Ok(self)
    }

    /// Add a Base58 encoded public key for the mint account
    pub fn add_spl_token(mut self, spl_token: &str) -> SolanaPayResult<Self> {
        if self.spl_token.is_some() {
            return Err(SolanaPayError::SplTokenAlreadyExists);
        }

        let public_key = PublicKey::from_base58(spl_token)?;

        self.spl_token.replace(public_key);

        Ok(self)
    }

    /// Multiple reference fields are allowed as optional query parameters. The values must be base58-encoded 32 byte arrays.
    /// These may or may not be public keys, on or off the curve, and may or may not correspond with accounts on Solana.
    /// Because Solana validators index transactions by these account keys,
    /// reference values can be used as client IDs (IDs usable before knowing the eventual payment transaction).
    /// The getSignaturesForAddress RPC method can be used locate transactions this way.
    pub fn add_reference(mut self, base58_reference: &str) -> SolanaPayResult<Self> {
        if self.references.len() > 254 {
            return Err(SolanaPayError::TooManyReferences);
        }
        let reference = Reference::from_base58(base58_reference)?;

        self.references.push(reference);

        self.references.dedup();

        Ok(self)
    }

    /// Same as [SolanaPayUrl::add_reference] above but allows adding multiple references at once
    pub fn add_reference_multiple(mut self, base58_references: &[&str]) -> SolanaPayResult<Self> {
        for base58_reference in base58_references {
            let reference = Reference::from_base58(base58_reference)?;

            self.references.push(reference);
        }

        self.references.dedup();

        Ok(self)
    }

    /// Add a UTF-8 URL label
    pub fn add_label(mut self, label: &'a str) -> SolanaPayResult<Self> {
        if self.label.is_some() {
            return Err(SolanaPayError::LabelAlreadyExists);
        }

        self.label.replace(Cow::Borrowed(label));

        Ok(self)
    }

    /// Add a UTF-8 URL message
    pub fn add_message(mut self, message: &'a str) -> SolanaPayResult<Self> {
        if self.message.is_some() {
            return Err(SolanaPayError::MessageAlreadyExists);
        }

        self.message.replace(Cow::Borrowed(message));

        Ok(self)
    }

    /// Add a UTF-8 URL memo to be included in the SPL memo part of a transaction
    pub fn add_spl_memo(mut self, spl_memo: &'a str) -> SolanaPayResult<Self> {
        if self.spl_memo.is_some() {
            return Err(SolanaPayError::MemoAlreadyExists);
        }

        self.spl_memo.replace(Cow::Borrowed(spl_memo));

        Ok(self)
    }

    /// Convert [Self] to a Solana Pay  URL
    pub fn to_url(&self) -> String {
        let outcome = String::from(SOLANA_SCHEME)
            + &self.recipient.to_base58()
            + "?"
            + &self.prepare_amount()
            + &self.prepare_spl_token()
            + &self.prepare_references()
            + &self.prepare_label()
            + &self.prepare_message()
            + &self.prepare_spl_memo();

        outcome.replace("?&", "?").replace("??", "?")
    }

    async fn resolve_decimals<F: Fn([u8; 32]) -> Fut, Fut: Future<Output = u8> + Send + 'static>(
        &self,
        lookup_fn: F,
    ) -> SolanaPayResult<()> {
        let mint_decimals = lookup_fn(self.spl_token.unwrap().to_bytes()).await; //Unwrap since the spl-token must exist at this point

        self.amount.as_ref().map_or(Ok(()), |amount_exists| {
            if (amount_exists.leading_zeroes + amount_exists.significant_digits_count)
                > mint_decimals as usize
            {
                Err(SolanaPayError::NumberOfDecimalsExceedsMintConfiguration)
            } else {
                Ok(())
            }
        })
    }

    fn prepare_optional_value_with_encoding(
        &self,
        name: &str,
        optional_value: Option<&Cow<str>>,
    ) -> String {
        if let Some(value) = optional_value.as_ref() {
            let encoded = utf8_percent_encode(value, NON_ALPHANUMERIC).to_string();

            String::new() + "&" + name + "=" + &encoded
        } else {
            String::default()
        }
    }

    fn prepare_amount(&self) -> String {
        if let Some(amount) = self.amount.as_ref() {
            String::new() + "?" + "amount=" + amount.as_string
        } else {
            String::default()
        }
    }

    fn prepare_spl_token(&self) -> String {
        if let Some(spl_token) = self.spl_token.as_ref() {
            String::new() + "&" + "spl-token=" + &spl_token.to_base58()
        } else {
            String::default()
        }
    }

    fn prepare_references(&self) -> String {
        let mut outcome = String::default();

        self.references.iter().for_each(|reference| {
            outcome.push('&');
            outcome.push_str("reference=");
            outcome.push_str(&reference.to_base58());
        });

        outcome
    }

    fn prepare_label(&self) -> String {
        self.prepare_optional_value_with_encoding("label", self.label.as_ref())
    }

    fn prepare_message(&self) -> String {
        self.prepare_optional_value_with_encoding("message", self.message.as_ref())
    }

    fn prepare_spl_memo(&self) -> String {
        self.prepare_optional_value_with_encoding("memo", self.spl_memo.as_ref())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum QueryParam {
    Amount,
    SplToken,
    Reference,
    Label,
    Message,
    SplMemo,
    Unsupported,
}

impl TryFrom<&str> for QueryParam {
    type Error = SolanaPayError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let outcome = match value {
            "amount" => Self::Amount,
            "spl-token" => Self::SplToken,
            "reference" => Self::Reference,
            "label" => Self::Label,
            "message" => Self::Message,
            "memo" => Self::SplMemo,
            _ => Self::Unsupported,
        };

        Ok(outcome)
    }
}

#[cfg(test)]
mod url_parsing_checks {
    use crate::*;
    #[test]
    fn parse_1_sol() {
        let transfer_1_sol = "solana:mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN?amount=1&label=Michael&message=Thanks%20for%20all%20the%20fish&memo=OrderId12345";
        let lookup_fn = |_value| async { 9 };
        let transfer_1_sol_decoded = smol::block_on(async {
            SolanaPayUrl::new()
                .parse(transfer_1_sol, lookup_fn)
                .await
                .unwrap()
        });

        let transfer_1_sol_other = SolanaPayUrl::default()
            .add_recipient("mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN")
            .unwrap()
            .add_amount("1")
            .unwrap()
            .add_label("Michael")
            .unwrap()
            .add_message("Thanks for all the fish")
            .unwrap()
            .add_spl_memo("OrderId12345")
            .unwrap();

        assert_eq!(transfer_1_sol_decoded, transfer_1_sol_other);

        let encode_again = transfer_1_sol_decoded.to_url();
        assert_eq!(&encode_again, transfer_1_sol);
    }

    #[test]
    fn parse_spl_token() {
        let zero_zero_one_usdc = "solana:mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN?amount=0.01&spl-token=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let lookup_fn = |_value| async { 6 };

        let decoded_zero_zero_one_usdc = smol::block_on(async {
            SolanaPayUrl::new()
                .parse(zero_zero_one_usdc, lookup_fn)
                .await
                .unwrap()
        });

        let zero_zero_one_usdc_other = SolanaPayUrl::default()
            .add_recipient("mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN")
            .unwrap()
            .add_amount("0.01")
            .unwrap()
            .add_spl_token("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")
            .unwrap();

        assert_eq!(decoded_zero_zero_one_usdc, zero_zero_one_usdc_other);

        let encode_again = decoded_zero_zero_one_usdc.to_url();
        assert_eq!(&encode_again, zero_zero_one_usdc);
    }

    #[test]
    fn prompt_amount() {
        let prompt_amount = "solana:mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN&label=Michael";

        let decoded_prompt_amount = smol::block_on(async {
            SolanaPayUrl::new()
                .parse(prompt_amount, Utils::native_sol)
                .await
                .unwrap()
        });

        let prompt_amount_other = SolanaPayUrl::default()
            .add_recipient("mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN")
            .unwrap()
            .add_label("Michael")
            .unwrap();

        assert_eq!(decoded_prompt_amount, prompt_amount_other);

        let encode_again = decoded_prompt_amount.to_url();
        assert_eq!(&encode_again, prompt_amount);
    }

    #[test]
    fn all_fields_encode_decode() {
        let all_fields = "solana:mvines9iiHiQTysrwkJjGf2gb9Ex9jXJX8ns3qwf2kN?amount=0.01&spl-token=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

        let lookup_fn = |_| async { 6 };

        let decoded_all_fields = smol::block_on(async {
            SolanaPayUrl::new()
                .parse(all_fields, lookup_fn)
                .await
                .unwrap()
                .add_reference("7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNatx")
                .unwrap()
                .add_reference_multiple(&[
                    "7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNatx",
                    "7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNatx",
                    "7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNatx",
                ])
                .unwrap()
        });

        assert!(decoded_all_fields.references.len() == 1);

        let decoded_all_fields = decoded_all_fields
            .add_reference_multiple(&[
                "7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNatx",
                "7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNaty",
                "7owWEdgJRWpKsiDFNU4qT2kgMe2kitPXem5Yy8VdNatz",
            ])
            .unwrap();
        assert!(decoded_all_fields.references.len() == 3);
    }
}
