// VimDesignCppTest — validates that C++ can call into VimDesignLib through
// the generated C ABI header (docs/ARCHITECTURE.md §9, §12).

#include <gtest/gtest.h>

#include <cstring>

#include "vim_design.h"

TEST(VimDesignFfi, VersionIsNonEmpty) {
    const char* version = vim_version();
    ASSERT_NE(version, nullptr);
    EXPECT_GT(std::strlen(version), 0u);
    // Placeholder skeleton ships as 0.x.y — just require a dotted version.
    EXPECT_NE(std::strchr(version, '.'), nullptr);
}

TEST(VimDesignFfi, CreateDestroyRoundtrip) {
    VimDesign* handle = nullptr;
    ASSERT_EQ(vim_create(&handle), VimStatus_Ok);
    ASSERT_NE(handle, nullptr);
    EXPECT_EQ(vim_destroy(handle), VimStatus_Ok);
}

TEST(VimDesignFfi, NullArgumentsAreRejectedNotFatal) {
    EXPECT_EQ(vim_create(nullptr), VimStatus_NullArgument);
    EXPECT_EQ(vim_destroy(nullptr), VimStatus_NullArgument);
}

TEST(VimDesignFfi, MultipleDocumentsAreIndependent) {
    VimDesign* a = nullptr;
    VimDesign* b = nullptr;
    ASSERT_EQ(vim_create(&a), VimStatus_Ok);
    ASSERT_EQ(vim_create(&b), VimStatus_Ok);
    EXPECT_NE(a, b);
    EXPECT_EQ(vim_destroy(a), VimStatus_Ok);
    EXPECT_EQ(vim_destroy(b), VimStatus_Ok);
}
