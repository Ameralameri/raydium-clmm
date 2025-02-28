use crate::states::*;
use anchor_lang::prelude::*;
use anchor_spl::{
    token::{self, Token},
    token_2022::{
        self,
        spl_token_2022::{
            self,
            extension::{
                confidential_transfer::ConfidentialTransferMint,
                default_account_state::DefaultAccountState, non_transferable::NonTransferable,
                permanent_delegate::get_permanent_delegate, transfer_fee::TransferFeeConfig,
                transfer_fee::MAX_FEE_BASIS_POINTS, BaseStateWithExtensions, StateWithExtensions,
            },
        },
    },
    token_interface::{Mint, TokenAccount},
};

pub fn transfer_from_user_to_pool_vault<'info>(
    signer: &Signer<'info>,
    from: &InterfaceAccount<'info, TokenAccount>,
    to_vault: &InterfaceAccount<'info, TokenAccount>,
    mint: Option<InterfaceAccount<'info, Mint>>,
    token_program: &AccountInfo<'info>,
    token_program_2022: Option<AccountInfo<'info>>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let mut token_program_info = token_program.to_account_info();
    let from_token_info = from.to_account_info();
    match (mint, token_program_2022) {
        (Some(mint), Some(token_program_2022)) => {
            if from_token_info.owner == token_program_2022.key {
                token_program_info = token_program_2022.to_account_info()
            }
            token_2022::transfer_checked(
                CpiContext::new(
                    token_program_info,
                    token_2022::TransferChecked {
                        from: from_token_info,
                        to: to_vault.to_account_info(),
                        authority: signer.to_account_info(),
                        mint: mint.to_account_info(),
                    },
                ),
                amount,
                mint.decimals,
            )
        }
        _ => token::transfer(
            CpiContext::new(
                token_program_info,
                token::Transfer {
                    from: from_token_info,
                    to: to_vault.to_account_info(),
                    authority: signer.to_account_info(),
                },
            ),
            amount,
        ),
    }
}

pub fn transfer_from_pool_vault_to_user<'info>(
    pool_state_loader: &AccountLoader<'info, PoolState>,
    from_vault: &InterfaceAccount<'info, TokenAccount>,
    to: &InterfaceAccount<'info, TokenAccount>,
    mint: Option<InterfaceAccount<'info, Mint>>,
    token_program: &AccountInfo<'info>,
    token_program_2022: Option<AccountInfo<'info>>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let mut token_program_info = token_program.to_account_info();
    let from_vault_info = from_vault.to_account_info();
    match (mint, token_program_2022) {
        (Some(mint), Some(token_program_2022)) => {
            if from_vault_info.owner == token_program_2022.key {
                token_program_info = token_program_2022.to_account_info()
            }
            token_2022::transfer_checked(
                CpiContext::new_with_signer(
                    token_program_info,
                    token_2022::TransferChecked {
                        from: from_vault_info,
                        to: to.to_account_info(),
                        authority: pool_state_loader.to_account_info(),
                        mint: mint.to_account_info(),
                    },
                    &[&pool_state_loader.load()?.seeds()],
                ),
                amount,
                mint.decimals,
            )
        }
        _ => token::transfer(
            CpiContext::new_with_signer(
                token_program_info,
                token::Transfer {
                    from: from_vault_info,
                    to: to.to_account_info(),
                    authority: pool_state_loader.to_account_info(),
                },
                &[&pool_state_loader.load()?.seeds()],
            ),
            amount,
        ),
    }
}

pub fn close_spl_account<'a, 'b, 'c, 'info>(
    owner: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
    close_account: &InterfaceAccount<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    // token_program_2022: &Program<'info, Token2022>,
    signers_seeds: &[&[&[u8]]],
) -> Result<()> {
    let token_program_info = token_program.to_account_info();
    let close_account_info = close_account.to_account_info();
    // if close_account_info.owner == token_program_2022.key {
    //     token_program_info = token_program_2022.to_account_info()
    // }

    token_2022::close_account(CpiContext::new_with_signer(
        token_program_info,
        token_2022::CloseAccount {
            account: close_account_info,
            destination: destination.to_account_info(),
            authority: owner.to_account_info(),
        },
        signers_seeds,
    ))
}

pub fn burn<'a, 'b, 'c, 'info>(
    owner: &Signer<'info>,
    mint: &InterfaceAccount<'info, Mint>,
    burn_account: &InterfaceAccount<'info, TokenAccount>,
    token_program: &Program<'info, Token>,
    // token_program_2022: &Program<'info, Token2022>,
    signers_seeds: &[&[&[u8]]],
    amount: u64,
) -> Result<()> {
    let mint_info = mint.to_account_info();
    let token_program_info: AccountInfo<'_> = token_program.to_account_info();
    // if mint_info.owner == token_program_2022.key {
    //     token_program_info = token_program_2022.to_account_info()
    // }
    token_2022::burn(
        CpiContext::new_with_signer(
            token_program_info,
            token_2022::Burn {
                mint: mint_info,
                from: burn_account.to_account_info(),
                authority: owner.to_account_info(),
            },
            signers_seeds,
        ),
        amount,
    )
}

/// Calculate the fee for output amount
pub fn get_transfer_inverse_fee(
    mint_account: InterfaceAccount<Mint>,
    post_fee_amount: u64,
) -> Result<u64> {
    let mint_info = mint_account.to_account_info();
    if *mint_info.owner == Token::id() {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;

    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        let epoch = Clock::get()?.epoch;

        let transfer_fee = transfer_fee_config.get_epoch_fee(epoch);
        if u16::from(transfer_fee.transfer_fee_basis_points) == MAX_FEE_BASIS_POINTS {
            u64::from(transfer_fee.maximum_fee)
        } else {
            transfer_fee_config
                .calculate_inverse_epoch_fee(epoch, post_fee_amount)
                .unwrap()
        }
    } else {
        0
    };
    Ok(fee)
}

/// Calculate the fee for input amount
pub fn get_transfer_fee(mint_account: InterfaceAccount<Mint>, pre_fee_amount: u64) -> Result<u64> {
    let mint_info = mint_account.to_account_info();
    if *mint_info.owner == Token::id() {
        return Ok(0);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;

    let fee = if let Ok(transfer_fee_config) = mint.get_extension::<TransferFeeConfig>() {
        transfer_fee_config
            .calculate_epoch_fee(Clock::get()?.epoch, pre_fee_amount)
            .unwrap()
    } else {
        0
    };
    Ok(fee)
}

pub fn is_supported_mint(mint_account: &InterfaceAccount<Mint>) -> Result<bool> {
    let mint_info = mint_account.to_account_info();
    if *mint_info.owner == Token::id() {
        return Ok(true);
    }
    let mint_data = mint_info.try_borrow_data()?;
    let mint = StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;

    if mint.get_extension::<NonTransferable>().is_ok() {
        return Ok(false);
    }
    if mint.get_extension::<DefaultAccountState>().is_ok() {
        return Ok(false);
    }
    let maybe_permanent_delegate = get_permanent_delegate(&mint);
    if maybe_permanent_delegate.is_some() {
        return Ok(false);
    }

    if mint.get_extension::<ConfidentialTransferMint>().is_ok() {
        return Ok(false);
    }

    Ok(true)
}
