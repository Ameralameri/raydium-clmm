use crate::error::ErrorCode;
use crate::libraries::tick_math;
use crate::swap::swap_internal;
use crate::util::*;
use crate::{states::*, util};
use anchor_lang::prelude::*;
use anchor_spl::token::Token;
use anchor_spl::token_interface::{Mint, Token2022, TokenAccount};
use std::collections::VecDeque;
#[derive(Accounts)]
pub struct SwapSingleV2<'info> {
    /// The user performing the swap
    pub payer: Signer<'info>,

    /// The factory state to read protocol fees
    #[account(address = pool_state.load()?.amm_config)]
    pub amm_config: Box<Account<'info, AmmConfig>>,

    /// The program account of the pool in which the swap will be performed
    #[account(mut)]
    pub pool_state: AccountLoader<'info, PoolState>,

    /// The user token account for input token
    #[account(mut)]
    pub input_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The user token account for output token
    #[account(mut)]
    pub output_token_account: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The vault token account for input token
    #[account(mut)]
    pub input_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The vault token account for output token
    #[account(mut)]
    pub output_vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The program account for the most recent oracle observation
    #[account(mut, address = pool_state.load()?.observation_key)]
    pub observation_state: AccountLoader<'info, ObservationState>,

    /// SPL program for token transfers
    pub token_program: Program<'info, Token>,

    /// SPL program 2022 for token transfers
    pub token_program_2022: Program<'info, Token2022>,

    /// CHECK:
    // #[account(
    //     address = spl_memo::id()
    // )]
    pub memo_program: UncheckedAccount<'info>,

    /// The mint of token vault 0
    #[account(
        address = input_vault.mint
    )]
    pub input_vault_mint: Box<InterfaceAccount<'info, Mint>>,

    /// The mint of token vault 1
    #[account(
        address = output_vault.mint
    )]
    pub output_vault_mint: Box<InterfaceAccount<'info, Mint>>,
    // remaining accounts
    // tick_array_account_1
    // tick_array_account_2
    // tick_array_account_...
}

/// Performs a single exact input/output swap
/// if is_base_input = true, return vaule is the max_amount_out, otherwise is min_amount_in
pub fn exact_internal_v2<'info>(
    ctx: &mut SwapSingleV2<'info>,
    remaining_accounts: &[AccountInfo<'info>],
    amount_specified: u64,
    sqrt_price_limit_x64: u128,
    is_base_input: bool,
) -> Result<u64> {
    let block_timestamp = solana_program::clock::Clock::get()?.unix_timestamp as u64;

    let amount_0;
    let amount_1;
    let zero_for_one;
    let swap_price_before;

    let input_balance_before = ctx.input_vault.amount;
    let output_balance_before = ctx.output_vault.amount;

    let mut transfer_fee = 0;
    if is_base_input {
        transfer_fee = util::get_transfer_fee(*ctx.input_vault_mint.clone(), amount_specified).unwrap();
    }

    {
        swap_price_before = ctx.pool_state.load()?.sqrt_price_x64;
        let pool_state = &mut ctx.pool_state.load_mut()?;
        zero_for_one = ctx.input_vault.mint == pool_state.token_mint_0;

        require_gt!(block_timestamp, pool_state.open_time);

        require!(
            if zero_for_one {
                ctx.input_vault.key() == pool_state.token_vault_0
                    && ctx.output_vault.key() == pool_state.token_vault_1
            } else {
                ctx.input_vault.key() == pool_state.token_vault_1
                    && ctx.output_vault.key() == pool_state.token_vault_0
            },
            ErrorCode::InvalidInputPoolVault
        );

        let tick_array_states = &mut VecDeque::new();
        for tick_array_info in remaining_accounts {
            tick_array_states.push_back(TickArrayState::load_mut(tick_array_info)?);
        }

        (amount_0, amount_1) = swap_internal(
            &ctx.amm_config,
            pool_state,
            tick_array_states,
            &mut ctx.observation_state.load_mut()?,
            amount_specified - transfer_fee,
            if sqrt_price_limit_x64 == 0 {
                if zero_for_one {
                    tick_math::MIN_SQRT_PRICE_X64 + 1
                } else {
                    tick_math::MAX_SQRT_PRICE_X64 - 1
                }
            } else {
                sqrt_price_limit_x64
            },
            zero_for_one,
            is_base_input,
            oracle::block_timestamp(),
        )?;

        #[cfg(feature = "enable-log")]
        msg!(
            "exact_swap_internal, is_base_input:{}, amount_0: {}, amount_1: {}",
            is_base_input,
            amount_0,
            amount_1
        );
        require!(
            amount_0 != 0 && amount_1 != 0,
            ErrorCode::TooSmallInputOrOutputAmount
        );
    }
    let (token_account_0, token_account_1, vault_0, vault_1, vault_0_mint, vault_1_mint) =
        if zero_for_one {
            (
                ctx.input_token_account.clone(),
                ctx.output_token_account.clone(),
                ctx.input_vault.clone(),
                ctx.output_vault.clone(),
                ctx.input_vault_mint.clone(),
                ctx.output_vault_mint.clone(),
            )
        } else {
            (
                ctx.output_token_account.clone(),
                ctx.input_token_account.clone(),
                ctx.output_vault.clone(),
                ctx.input_vault.clone(),
                ctx.output_vault_mint.clone(),
                ctx.input_vault_mint.clone(),
            )
        };

    if zero_for_one {
        if !is_base_input {
            transfer_fee = util::get_transfer_inverse_fee(*ctx.input_vault_mint.clone(), amount_0).unwrap();
        }
        //  x -> y, deposit x token from user to pool vault.
        transfer_from_user_to_pool_vault(
            &ctx.payer,
            &token_account_0,
            &vault_0,
            Some(*vault_0_mint),
            &ctx.token_program,
            Some(ctx.token_program_2022.to_account_info()),
            amount_0 + transfer_fee,
        )?;
        if vault_1.amount <= amount_1 {
            // freeze pool, disable all instructions
            ctx.pool_state.load_mut()?.set_status(255);
        }
        // x -> y，transfer y token from pool vault to user.
        transfer_from_pool_vault_to_user(
            &ctx.pool_state,
            &vault_1,
            &token_account_1,
            Some(*vault_1_mint),
            &ctx.token_program,
            Some(ctx.token_program_2022.to_account_info()),
            amount_1,
        )?;
    } else {
        if !is_base_input {
            transfer_fee = util::get_transfer_inverse_fee(*ctx.input_vault_mint.clone(), amount_1).unwrap();
        }
        transfer_from_user_to_pool_vault(
            &ctx.payer,
            &token_account_1,
            &vault_1,
            Some(*vault_1_mint),
            &ctx.token_program,
            Some(ctx.token_program_2022.to_account_info()),
            amount_1 + transfer_fee,
        )?;
        if vault_0.amount <= amount_0 {
            // freeze pool, disable all instructions
            ctx.pool_state.load_mut()?.set_status(255);
        }
        transfer_from_pool_vault_to_user(
            &ctx.pool_state,
            &vault_0,
            &token_account_0,
            Some(*vault_0_mint),
            &ctx.token_program,
            Some(ctx.token_program_2022.to_account_info()),
            amount_0,
        )?;
    }
    ctx.output_vault.reload()?;
    ctx.input_vault.reload()?;

    let pool_state = ctx.pool_state.load()?;
    emit!(SwapEvent {
        pool_state: pool_state.key(),
        sender: ctx.payer.key(),
        token_account_0: token_account_0.key(),
        token_account_1: token_account_1.key(),
        amount_0,
        amount_1,
        zero_for_one,
        sqrt_price_x64: pool_state.sqrt_price_x64,
        liquidity: pool_state.liquidity,
        tick: pool_state.tick_current
    });
    if zero_for_one {
        require_gt!(swap_price_before, pool_state.sqrt_price_x64);
    } else {
        require_gt!(pool_state.sqrt_price_x64, swap_price_before);
    }

    if is_base_input {
        Ok(output_balance_before
            .checked_sub(ctx.output_vault.amount)
            .unwrap())
    } else {
        Ok(ctx
            .input_vault
            .amount
            .checked_sub(input_balance_before)
            .unwrap())
    }
}

pub fn swap_v2<'a, 'b, 'c, 'info>(
    ctx: Context<'a, 'b, 'c, 'info, SwapSingleV2<'info>>,
    amount: u64,
    other_amount_threshold: u64,
    sqrt_price_limit_x64: u128,
    is_base_input: bool,
) -> Result<()> {
    let amount_result = exact_internal_v2(
        ctx.accounts,
        ctx.remaining_accounts,
        amount,
        sqrt_price_limit_x64,
        is_base_input,
    )?;
    if is_base_input {
        require_gte!(
            amount_result,
            other_amount_threshold,
            ErrorCode::TooLittleOutputReceived
        );
    } else {
        require_gte!(
            other_amount_threshold,
            amount_result,
            ErrorCode::TooMuchInputPaid
        );
    }

    Ok(())
}
