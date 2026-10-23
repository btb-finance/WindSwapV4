// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity >=0.5.0;

/// @title Wind Swap Post-Swap Hook Interface
/// @notice Any contract set as the factory hook must implement this interface
/// @dev Called by CLPool after every swap. The hook can interact with OTHER pools
/// but cannot re-enter the calling pool (it remains locked during the hook call).
interface IWindHook {
    /// @notice Called after every swap on any WindSwap pool
    /// @dev Implementation should fail silently (use try/catch internally) if no
    /// profitable action is available. The calling pool wraps this in try/catch
    /// so a revert here will NOT revert the original swap.
    /// @param pool The pool where the swap just happened
    function afterSwap(address pool) external;
}
