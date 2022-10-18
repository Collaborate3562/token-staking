#![cfg_attr(not(feature = "std"), no_std)]

use concordium_cis2::*;
use concordium_std::*;

const TOKEN_ID: ContractTokenId = TokenIdUnit();

type ContractTokenId = TokenIdUnit;
type ContractTokenAmount = TokenAmountU64;

pub const OPERATOR_OF_ENTRYPOINT_NAME: &str = "operatorOf";
pub const BALANCE_OF_ENTRYPOINT_NAME: &str = "balanceOf";
pub const TRANSFER_ENTRYPOINT_NAME: &str = "transfer";

type ContractBalanceOfQueryParams = BalanceOfQueryParams<ContractTokenId>;
type ContractBalanceOfQueryResponse = BalanceOfQueryResponse<ContractTokenAmount>;
type TransferParameter = TransferParams<ContractTokenId, ContractTokenAmount>;

pub const SECOND_PER_YEAR: &u64 = &(365 * 24 * 60 * 60);

#[derive(Clone, Serialize, SchemaType)]
struct StakeState {
    amount: u64,
    staked_start_at: u64
}

impl StakeState {
    fn empty() -> Self {
        StakeState {
            amount: 0u64,
            staked_start_at: 0u64
        }
    }
}

#[derive(Serial, DeserialWithState, StateClone)]
#[concordium(state_parameter = "S")]
struct State<S> {
    stake:        StateMap<AccountAddress, StakeState, S>,
    total_staked: u64
}

#[derive(Serialize, Debug, PartialEq, Eq, Reject, SchemaType)]
enum CustomContractError {
    #[from(ParseError)]
    ParseParams,
    Cis2ClientError(Cis2ClientError),
    TokenNotFound,
    TokenAlreadyStaked,
    InvokeContractError,
    NoBalance,
    NotOperator,
}
#[derive(Serialize, Debug, PartialEq, Eq, Reject)]
pub enum Cis2ClientError {
    InvokeContractError,
    ParseParams,
    ParseResult,
}

type ContractError = Cis2Error<CustomContractError>;

type ContractResult<A> = Result<A, ContractError>;

/// Mapping errors related to contract invocations to CustomContractError.
impl<T> From<CallContractError<T>> for CustomContractError {
    fn from(_cce: CallContractError<T>) -> Self { Self::InvokeContractError }
}

impl From<CustomContractError> for ContractError {
    fn from(c: CustomContractError) -> Self { Cis2Error::Custom(c) }
}

impl<S: HasStateApi> State<S> {
    /// Creates a new state with no tokens.
    fn empty(state_builder: &mut StateBuilder<S>) -> Self {
        State {
            stake:        state_builder.new_map(),
            total_staked: 0u64,
        }
    }

    fn insert_token(
        &mut self,
        owner: &AccountAddress,
        amount: u64,
        staked_time: u64,
        state_builder: &mut StateBuilder<S>,
    ) -> ContractResult<()> {
        let mut owner_state =
            self.stake.entry(*owner).or_insert_with(|| StakeState::empty());
        owner_state.amount = amount;
        owner_state.staked_start_at = staked_time;
        self.total_staked += amount;
        Ok(())
    }

    fn remove_token(
        &mut self,
        owner: &AccountAddress,
        state_builder: &mut StateBuilder<S>,
    ) -> ContractResult<()> {
        let mut owner_state =
            self.stake.entry(*owner).or_insert_with(|| StakeState::empty());
        self.total_staked -= owner_state.amount;
        owner_state.amount = 0u64;
        owner_state.staked_start_at = 0u64;
        Ok(())
    }

    fn get_time(
        &mut self,
        owner: &AccountAddress,
        curr_time: u64,
        state_builder: &mut StateBuilder<S>,
    ) -> ContractResult<u64> {
        let mut owner_state =
            self.stake.entry(*owner).or_insert_with(|| StakeState::empty());
        Ok((curr_time - owner_state.staked_start_at) / &1000u64)
    }

    fn get_reward(
        &mut self,
        owner: &AccountAddress,
        time: u64,
        state_builder: &mut StateBuilder<S>,
    ) -> ContractResult<u64> {
        let mut owner_state =
            self.stake.entry(*owner).or_insert_with(|| StakeState::empty());
        
        Ok(owner_state.amount * time / SECOND_PER_YEAR)
    }
}

#[init(contract = "token-staking")]
fn contract_init<S: HasStateApi>(
    _ctx: &impl HasInitContext,
    state_builder: &mut StateBuilder<S>,
) -> InitResult<State<S>> {
    // Construct the initial contract state.
    Ok(State::empty(state_builder))
}

#[derive(Serial, Deserial, SchemaType)]
struct StakeParams {
    owner:  AccountAddress,
    amount: u64,
    token_contract_address: ContractAddress
}

#[receive(
    contract = "token",
    name = "stake",
    parameter = "StakeParams",
    error = "ContractError",
    mutable
)]
fn stake_token<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<()> {
    let params: StakeParams = ctx.parameter_cursor().get()?;
    let sender = ctx.sender();

    ensure!(sender.matches_account(&params.owner), ContractError::Unauthorized);
    ensure_balance(host, TOKEN_ID, &params.token_contract_address, params.amount, ctx)?;
    ensure_is_operator(host, ctx, &params.token_contract_address)?;

    let (state, builder) = host.state_and_builder();
    let slot_time = ctx.metadata().slot_time();
    state.insert_token(&params.owner, params.amount, Timestamp::timestamp_millis(&slot_time), builder)?;

    Ok(())
}

#[derive(Serial, Deserial, SchemaType)]
struct UnStakeParams {
    owner:  AccountAddress,
    token_contract_address: ContractAddress
}

#[receive(
    contract = "token-staking",
    name = "unstake",
    parameter = "UnStakeParams",
    error = "ContractError",
    mutable
)]
fn unstake_token<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<()> {
    let params: UnStakeParams = ctx.parameter_cursor().get()?;
    let sender = ctx.sender();

    ensure!(sender.matches_account(&params.owner), ContractError::Unauthorized);

    let (state, builder) = host.state_and_builder();
    state.remove_token(&params.owner, builder)?;

    let reward = calculate_reward(host, ctx, &params.owner).unwrap();
    Cis2Client::transfer(
        host,
        TOKEN_ID,
        params.token_contract_address,
        concordium_cis2::TokenAmountU64(reward),
        ctx.owner(),
        concordium_cis2::Receiver::Account(ctx.invoker()),
    );

    Ok(())
}

#[derive(Serial, Deserial, SchemaType)]
struct ClaimParams {
    owner:  AccountAddress,
    token_contract_address: ContractAddress
}

#[receive(
    contract = "token-staking",
    name = "claim",
    parameter = "ClaimParams",
    error = "ContractError",
    mutable
)]
fn claim_reward<S: HasStateApi>(
    ctx: &impl HasReceiveContext,
    host: &mut impl HasHost<State<S>, StateApiType = S>,
) -> ContractResult<()> {
    let params: ClaimParams = ctx.parameter_cursor().get()?;
    let sender = ctx.sender();

    ensure!(sender.matches_account(&params.owner), ContractError::Unauthorized);

    let (state, builder) = host.state_and_builder();
    state.remove_token(&params.owner, builder)?;

    let reward = calculate_reward(host, ctx, &params.owner).unwrap();
    Cis2Client::transfer(
        host,
        TOKEN_ID,
        params.token_contract_address,
        concordium_cis2::TokenAmountU64(reward),
        ctx.owner(),
        concordium_cis2::Receiver::Account(ctx.invoker()),
    );

    Ok(())
}

fn calculate_reward<S: HasStateApi>(
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    ctx: &impl HasReceiveContext<()>,
    owner: &AccountAddress,
) -> Result<u64, CustomContractError> {
    let slot_time = ctx.metadata().slot_time();
    let (state, state_builder) = host.state_and_builder();
    let time = state.get_time(owner, concordium_std::Timestamp::timestamp_millis(&slot_time), state_builder).unwrap();
    let reward = state.get_reward(owner, time, state_builder).unwrap();

    Ok(reward)
}

pub struct Cis2Client;

impl Cis2Client {
    pub(crate) fn is_operator_of<S: HasStateApi>(
        host: &mut impl HasHost<State<S>, StateApiType = S>,
        owner: Address,
        current_contract_address: ContractAddress,
        token_contract_address: &ContractAddress,
    ) -> Result<bool, Cis2ClientError> {
        let params = &OperatorOfQueryParams {
            queries: vec![OperatorOfQuery {
                owner,
                address: Address::Contract(current_contract_address),
            }],
        };

        let parsed_res: OperatorOfQueryResponse = Cis2Client::invoke_contract_read_only(
            host,
            token_contract_address,
            OPERATOR_OF_ENTRYPOINT_NAME,
            params,
        )?;

        let is_operator = parsed_res
            .0
            .first()
            .ok_or(Cis2ClientError::InvokeContractError)?
            .to_owned();

        Ok(is_operator)
    }

    pub(crate) fn has_balance<S: HasStateApi>(
        host: &mut impl HasHost<State<S>, StateApiType = S>,
        token_id: ContractTokenId,
        token_contract_address: &ContractAddress,
        amount: u64,
        owner: Address,
    ) -> Result<bool, Cis2ClientError> {
        let params = ContractBalanceOfQueryParams {
            queries: vec![BalanceOfQuery {
                token_id,
                address: owner,
            }],
        };

        let parsed_res: ContractBalanceOfQueryResponse = Cis2Client::invoke_contract_read_only(
            host,
            token_contract_address,
            BALANCE_OF_ENTRYPOINT_NAME,
            &params,
        )?;

        let is_operator = parsed_res
            .0
            .first()
            .ok_or(Cis2ClientError::InvokeContractError)?
            .to_owned();

        Result::Ok(is_operator.cmp(&TokenAmountU64(amount)).is_ge())
    }

    pub(crate) fn transfer<S: HasStateApi>(
        host: &mut impl HasHost<State<S>, StateApiType = S>,
        token_id: TokenIdUnit,
        token_contract_address: ContractAddress,
        amount: ContractTokenAmount,
        from: AccountAddress,
        to: Receiver,
    ) -> Result<bool, Cis2ClientError> {
        let params: TransferParameter = TransferParams(vec![Transfer {
            token_id,
            amount,
            from: concordium_std::Address::Account(from),
            data: AdditionalData::empty(),
            to,
        }]);

        Cis2Client::invoke_contract_read_only(
            host,
            &token_contract_address,
            TRANSFER_ENTRYPOINT_NAME,
            &params,
        )?;

        Result::Ok(true)
    }
    
    fn invoke_contract_read_only<S: HasStateApi, R: Deserial, P: Serial>(
        host: &mut impl HasHost<State<S>, StateApiType = S>,
        contract_address: &ContractAddress,
        entrypoint_name: &str,
        params: &P,
    ) -> Result<R, Cis2ClientError> {
        let invoke_contract_result = host
            .invoke_contract_read_only(
                contract_address,
                params,
                EntrypointName::new(entrypoint_name).unwrap_abort(),
                Amount::from_ccd(0),
            )
            .map_err(|_e| Cis2ClientError::InvokeContractError)?;
        let mut invoke_contract_res = match invoke_contract_result {
            Some(s) => s,
            None => return Result::Err(Cis2ClientError::InvokeContractError),
        };
        let parsed_res =
            R::deserial(&mut invoke_contract_res).map_err(|_e| Cis2ClientError::ParseResult)?;

        Ok(parsed_res)
    }
}

fn ensure_is_operator<S: HasStateApi>(
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    ctx: &impl HasReceiveContext<()>,
    token_contract_address: &ContractAddress,
) -> Result<(), CustomContractError> {
    let is_operator = Cis2Client::is_operator_of(
        host,
        ctx.sender(),
        ctx.self_address(),
        token_contract_address,
    )
    .map_err(CustomContractError::Cis2ClientError)?;
    ensure!(is_operator, CustomContractError::NotOperator);
    Ok(())
}

fn ensure_balance<S: HasStateApi>(
    host: &mut impl HasHost<State<S>, StateApiType = S>,
    token_id: ContractTokenId,
    token_contract_address: &ContractAddress,
    amount: u64,
    ctx: &impl HasReceiveContext<()>,
) -> Result<(), CustomContractError> {
    let has_balance = Cis2Client::has_balance(host, token_id, token_contract_address, amount, ctx.sender())
        .map_err(CustomContractError::Cis2ClientError)?;
    ensure!(has_balance, CustomContractError::NoBalance);
    Ok(())
}
